# Multi-Backend Architecture

Design exploration for supporting multiple named storage backends,
enabling migration between backends (e.g., local to S3) and
redundant storage across backends.

## Problem

The config allows exactly one backend (`[backend]` in config.toml).
Transitioning from local to S3 (or vice versa) has no supported
path. Data stored on the old backend becomes orphaned, and the user
must manually copy blobs and edit TOML to switch.

## Driving Use Case

A vault has real data on local storage. The user wants to shift to
S3. Today the options are: (a) manually copy the `.blu/data/`
directory contents into the S3 bucket with the right prefix, then
hand-edit config.toml, or (b) start fresh on S3 and re-sync
everything. Neither is acceptable.

## Codebase Exploration

A future session should investigate these areas before designing
the solution:

1. Every consumer of `Config.backend` (the singular field at
   `src/config.rs:71`) and how the single `StorageBackend` impl is
   constructed and threaded through calling code.

2. The blob write path (sync, encrypt): where the backend is
   called, how errors propagate, whether writes are batched or
   per-blob.

3. The blob read path (restore): how it resolves which backend to
   read from, whether it retries or falls back.

4. Index push/pull (`src/config.rs` push/pull methods): whether
   the index sync assumes a single remote, and how it would
   generalize to N remotes.

5. The `StorageBackend` trait (`src/storage.rs:45`): whether the
   trait shape (read, write, exists, delete) is sufficient for
   multi-backend orchestration, or whether it needs a higher-level
   coordinator.

6. Config deserialization: how `BackendConfig` is parsed from TOML
   (`src/config/backend.rs`), and what a backward-compatible
   migration from `[backend]` (singular) to `[backends.<name>]`
   (named map) looks like.

7. The `BlobBuffer` and `EncBlobReader` types in `src/blob.rs`:
   whether they hold or reference the backend, and what ownership
   changes would be needed.

## Design Questions

These questions should be answered during the exploration before
committing to an implementation plan:

1. Config format: `[backends.<name>]` map with a `default` flag?
   Or a `[[backends]]` array with a `name` field? The named-map
   approach reads better in TOML and avoids ordering ambiguity.

2. Write strategy: fan-out to all backends on every write? Or
   write to the default only, with explicit mirror/push to others?

3. Read strategy: prefer local backends for speed, fall back to
   remote? Or always read from the default? Should restore accept
   a `--backend` flag?

4. Migration command: something like `blu backend mirror` that
   copies all blobs from one named backend to another. This is the
   core operation for the driving use case.

5. Consistency: what if backends have different blob sets? Is there
   a reconciliation or diff command? Or is divergence simply the
   user's problem to resolve via mirror?

6. Index sync: one index across all backends, or per-backend index
   state? Indexes are encrypted and content-addressed, so the same
   index should be valid regardless of which backend holds the
   blobs.

7. Backward compatibility: existing single `[backend]` configs
   must keep working. Treat the old format as a sole backend named
   "default" (or similar).

8. Error handling: if a write to one backend succeeds but another
   fails, what is the behavior? Fail the whole operation? Warn and
   continue? Record partial state?

## Scope

This work covers the plumbing for N backends of any supported type.
It does not cover adding new backend types (Azure Blob Storage,
Google Cloud Storage, DigitalOcean Spaces). Those are independent
and can be added once the multi-backend plumbing exists.

## Git Remotes Model

The user experience should feel like git remotes. Named backends
are added, listed, mirrored between, and removed through a CLI
subcommand group. The parallel:

    git remote add origin <url>        blu backend add s3-prod --type s3 --bucket ...
    git remote add backup <url>        blu backend add local-archive --type local --path ...
    git remote -v                      blu backend list
    git push origin                    blu sync --backend s3-prod
    git push --all                     blu sync --all-backends
    git pull origin                    blu pull --backend s3-prod
    git remote remove backup           blu backend remove local-archive

The migration workflow for the driving use case would be:

1. `blu backend add s3-prod --type s3 --bucket ... --region ...`
   Adds S3 as a named backend alongside the existing local.

2. `blu backend mirror --from local --to s3-prod`
   Copies all blobs from local storage to S3.

3. `blu backend set-default s3-prod`
   New writes and index pushes target S3.

4. `blu backend remove local` (optional, when confident)
   Removes the local backend config entry. Does not delete local
   blob files; that is a separate cleanup step.

Config evolution:

    Before (current):

    [backend]
    type = "local"
    path = ".blu/data"

    After (multi-backend):

    [backends.local]
    type = "local"
    path = ".blu/data"

    [backends.s3-prod]
    type = "s3"
    bucket = "my-bucket"
    region = "us-east-1"
    default = true

Backward compatibility: if `[backend]` (singular, no name) is
present and `[backends]` is absent, treat it as a sole backend
named "default." Emit a deprecation notice suggesting the user
migrate the config format.

This model gives the user explicit control over where data lives,
makes migration a first-class operation, and avoids surprising
behavior where writes silently fan out to backends the user may
have forgotten about.
