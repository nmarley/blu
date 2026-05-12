# Multi-Backend: Remaining Work

Follow-on enhancements to the multi-backend implementation.
The core feature (Phases 1-3) is complete.

## 1. Mirror dry-run

`blu backend mirror --from X --to Y --dry-run`

Walk the blob index, check existence on the destination, and print
what would be copied without transferring any data. Report total
blob count and estimated bytes.

Implementation: add `--dry-run` flag to `BackendMirrorArgs` in
`src/cli/clapargs.rs`. In `src/cli/backend_cmd.rs::mirror()`, skip
the `read_data`/`write_data` calls when the flag is set; still
accumulate the counts from `from_backend.read_data` size (or use
`from_backend.exists` only and report count without byte estimate).

## 2. Backend diff

`blu backend diff --from X --to Y`

Compare blob sets between two named backends. For each blob path in
the blob index, check `exists()` on both backends. Report:

- Blobs present in both
- Blobs present only in source
- Blobs present only in destination

This gives the user visibility into divergence before deciding
whether to mirror.

Implementation: new subcommand variant in `BackendCommand`, new
function in `src/cli/backend_cmd.rs`. Iterates
`BlobIndex.path_index` keys, calls `exists()` on each backend.

## 3. Backend list with blob counts

`blu backend list --stats`

For each configured backend, report the number of blobs present.
This requires listing objects in the backend, which may be slow
for S3 (paginated `ListObjectsV2`).

Implementation: add `--stats` flag to `BackendCommand::List`. For
local backends, walk the data directory and count files. For S3,
add a `list_objects` or `count` method to the `Backend` trait
(or keep it as a backend-specific helper, since the trait should
not mandate pagination details).

## 4. Mirror with tag filtering

`blu backend mirror --from X --to Y --tag <tag>`

Mirror only the blobs referenced by files matching a given tag.
Requires joining the tag index to the plain index to the blob
index to determine which blob paths are relevant.

This is a convenience for large vaults where the user wants to
migrate a subset of data to a secondary backend.

## Scope exclusions (unchanged)

These remain out of scope for the multi-backend feature:

- New backend types (Azure, GCS, DigitalOcean Spaces)
- Automatic fan-out writes
- Automatic read fallback between backends
- Per-backend encryption keys
