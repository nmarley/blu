# Pulse Check

Last updated: 2026-05-15

Version: 0.5.0 (pre-release, beta quality)

Tests: 195 passing, 0 failing, 2 ignored. Clippy clean.

## Overall Assessment

The cryptographic core (envelope encryption, PQ hybrid KEK wrapping,
ChaCha20-Poly1305 pipeline, agent daemon with mlock'd memory) is solid
and well-tested. The content-addressed storage model, named multi-backend
config system, and backend mirror/diff commands are polished. The CLI
surface layer has significant gaps: several commands are stubs, error
handling relies on bare unwraps, and zero CLI-layer tests exist.

## Area-by-Area Status

### Encryption Pipeline: SOLID

Envelope encryption, ChaCha20-Poly1305 bulk encryption, PQ hybrid KEK
wrapping (ML-KEM-768 + X25519), agent daemon with zeroize-on-drop. All
well-tested with 189+ passing tests concentrated in keys/, agent/, and
v2format modules.

### Storage Backends: SOLID

Named multi-backend config with clean serde tagging and legacy migration.
BackendKind enum dispatch (Local, AmazonS3) with six async methods:
read_data, write_data, exists, delete, write_to_path, read_from_path.
S3 can be used standalone without a local data backend; blob data goes
directly to whichever backend is configured (no mandatory local staging).
Indexes are always local-first, pushed/pulled via sync.

`backend mirror` and `backend diff` are polished: concurrent with
semaphore-bounded parallelism, progress bars, dry-run support, tag
filtering.

Missing: no `list` method on backends (relies entirely on index for blob
enumeration; index loss means no discovery). No `blu doctor` diagnostics.

### Status Command (`src/cli/status.rs`): PARTIAL

Works for basic new/deleted/renamed/modified detection in deep and
shallow modes, plus encrypted-chunks percentage. Self-admits "probably
has bugs" at line 12.

Missing:
- Files in PlainIndex not yet encrypted (TODO line 14)
- Aggregate stats: file count, bytes deduplicated, tag count (TODO line 16)
- Zero awareness of backend health, sync state, or remote reachability
- Full-file hash comparison for new files in shallow mode (TODO line 85)
- Potential divide-by-zero if total_chunks == 0

### Delete Files (`src/cli/delete_files.rs`): STUB (CRITICAL)

**This command is a no-op.** It loads indexes, prints some file info,
and returns `Ok(())` without modifying any index or storage. Users who
think they are deleting data are not.

Missing:
- No PlainIndex mutation
- No BlobIndex mutation
- No blob deletion from storage backends
- No tag cascade/cleanup
- Bare `.unwrap()` at line 31 (panic on inconsistent index)

### Defrag Blobs (`src/cli/defrag_blobs.rs`): STUB

68 lines total. Loads blob index, iterates paths, sums sizes, then does
nothing. The bin-packing algorithm is referenced but not implemented.
Both dry-run and live paths log and return. Bare `.unwrap()` at line 36.

### Search (`src/cli/search.rs` + `src/search.rs`): PARTIAL

Functional for basic substring search across filenames and tags. Rebuilds
the suffix-table search index from scratch on every invocation (not
persisted). `Box::leak` for `'static` lifetime (acceptable for CLI, not
for long-running processes). Two bare `.unwrap()` calls that panic on
non-UTF-8 paths or stale hashes.

Missing: no persistence, no fuzzy matching, no boolean/compound queries.

### Tag System (`src/tag.rs` + `src/cli/tagger.rs`): WORKING

The most complete secondary feature. Dual-map design for bidirectional
lookup. Add/remove/drop-all/search/list all implemented. Good test
coverage. Minor TODOs: advanced boolean query syntax, return types
(HashSet vs Vec, iterator vs Vec).

### Config Validation (`src/config.rs`): PARTIAL

Validates backends non-empty, default_backend key exists, legacy format
promotion. Nine tests covering round-trips and error cases.

Missing:
- No `blu_version` compatibility check
- No S3 field validation (bucket name, region)
- No local backend path validation (exists? writable?)
- `prune_deleted` and `prune_dangling` fields exist but are never
  read or acted upon anywhere
- Dead code: `KeyID` struct and `KeyType` enum appear unused

### Error Handling: GOOD CORE, ROUGH EDGES

`BluError` in `src/error.rs` has 22 variants, 9 From impls, covers
all major error categories. Well-structured with thiserror.

13 bare `.unwrap()` calls in production CLI code (outside test modules).
All follow the pattern of iterating index keys then calling
`.get(key).unwrap()` on the same index. Safe under normal conditions;
panics on corrupted or partially-written indexes. Should be replaced
with proper error propagation.

### Test Coverage: GAPS IN CLI

195 tests passing, heavily concentrated in crypto/keys/agent/format
modules. Zero tests for any CLI subcommand: status, delete, search,
defrag, sync, encrypt, restore, list, backend. The entire user-facing
surface is untested.

## Bare Unwraps in Production Code

| File | Line | Trigger |
|------|------|---------|
| sync.rs | 71 | File hash missing from index |
| sync.rs | 78 | Block hash missing from index |
| search.rs | 21 | Non-UTF-8 path |
| search.rs | 44 | Stale hash in search results |
| list_files.rs | 35 | File hash missing from sorted keys |
| encrypt_files.rs | 51 | File hash missing from index |
| encrypt_files.rs | 62 | Block hash missing from index |
| delete_files.rs | 31 | File hash missing from sorted keys |
| defrag_blobs.rs | 36 | Chunk hash missing from blob index |
| restore_files.rs | 62 | Invalid glob pattern |
| restore_files.rs | 174 | Empty paths set in fileref |
| restore_files.rs | 175 | Path with no filename component |
| restore_files.rs | 183 | Empty path iterator |

## Build Notes

- S3 dependency (aws-config, aws-sdk-s3) always compiled in; should be
  feature-gated (noted in Cargo.toml line 32)
- security-framework (macOS Touch ID) unconditionally compiled; should
  be feature-gated for Linux builds
- No CI, no changelog, no release process
