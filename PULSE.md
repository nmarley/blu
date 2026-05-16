# Pulse Check

Last updated: 2026-05-15

Version: 0.5.0 (pre-release, beta quality)

## Overall Assessment

The cryptographic core (envelope encryption, PQ hybrid KEK wrapping,
ChaCha20-Poly1305 pipeline, agent daemon with mlock'd memory) is solid
and well-tested. The content-addressed storage model, named multi-backend
config system, and backend mirror/diff commands are polished. The CLI
surface layer has improved: delete_files is functional, bare unwraps
have been replaced with proper error propagation (24 fixed), and dead
config code has been removed. Zero CLI-layer tests still exist.

## Area-by-Area Status

### Encryption Pipeline: SOLID

Envelope encryption, ChaCha20-Poly1305 bulk encryption, PQ hybrid KEK
wrapping (ML-KEM-768 + X25519), agent daemon with zeroize-on-drop. All
well-tested with tests concentrated in keys/, agent/, and v2format
modules.

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

### Status Command (`src/cli/status.rs`): WORKING

Shows new/deleted/renamed/modified file detection in deep and shallow
modes, plus a vault summary: file count, total size, dedup savings,
chunk count, blob file count, encryption percentage, pending GC count,
tag stats, and configured backends. Divide-by-zero on empty index is
guarded. Changes section uses consistent prefixed labels.

Missing:
- Files in PlainIndex not yet encrypted (TODO)
- Remote backend reachability check (would require async/network)
- Full-file hash comparison for new files in shallow mode

### Delete Files (`src/cli/delete_files.rs`): COMPLETE

Full end-to-end delete cascade: removes files from PlainIndex,
cascades orphaned blocks from PlainIndex, removes chunks from
BlobIndex, deletes fully-dead blobs from the storage backend, removes
tags, and persists all three indexes. Now async to support backend
I/O. Supports `--backend` flag for targeting a specific backend.

`BlobIndex::delete_chunk` correctly distinguishes fully-dead blobs
(all chunks removed, safe to delete from backend) from partially-dead
blobs (still have live chunks, left for defrag to repack). Six new
tests cover partial deletion, full deletion, drain semantics, error
cases, multi-blob scenarios, and end-to-end backend file removal.

### Defrag Blobs (`src/cli/defrag_blobs.rs`): STUB

Stub. Loads blob index, iterates paths, sums sizes, then does
nothing. The bin-packing algorithm is referenced but not implemented.
Both dry-run and live paths log and return.

### Search (`src/cli/search.rs` + `src/search.rs`): PARTIAL

Functional for basic substring search across filenames and tags. Rebuilds
the suffix-table search index from scratch on every invocation (not
persisted). `Box::leak` for `'static` lifetime (acceptable for CLI, not
for long-running processes). Bare unwraps replaced with proper error
propagation.

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

### Error Handling: GOOD

`BluError` in `src/error.rs` covers all major error categories with
`thiserror`. All bare `.unwrap()` calls in production CLI code have
been replaced with proper error propagation.

### Test Coverage: GAPS IN CLI

Tests are heavily concentrated in crypto/keys/agent/format modules.
The delete cascade in the blob module has tests covering BlobIndex
mutation, partial/full blob death, drain semantics, and end-to-end
backend file removal. Zero tests for CLI subcommand entry points:
status, delete, search, defrag, sync, encrypt, restore, list,
backend.

## Bare Unwraps in Production Code

All bare `.unwrap()` calls in production code have been replaced
with proper `BluError` propagation.

## Build Notes

- S3 dependency (aws-config, aws-sdk-s3) always compiled in; should be
  feature-gated (noted in Cargo.toml line 32)
- security-framework (macOS Touch ID) unconditionally compiled; should
  be feature-gated for Linux builds
- No CI, no changelog, no release process
