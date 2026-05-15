# Pulse Check

Last updated: 2026-05-15

Version: 0.5.0 (pre-release, beta quality)

Tests: 195 passing, 0 failing, 2 ignored. Clippy clean.

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
has bugs" at line 12. Divide-by-zero on empty index is now guarded.

Missing:
- Files in PlainIndex not yet encrypted (TODO line 14)
- Aggregate stats: file count, bytes deduplicated, tag count (TODO line 16)
- Zero awareness of backend health, sync state, or remote reachability
- Full-file hash comparison for new files in shallow mode (TODO line 85)

### Delete Files (`src/cli/delete_files.rs`): WORKING

Functional: removes files from PlainIndex, cascades orphaned blocks
from PlainIndex, removes tags, and persists all three indexes. Bare
unwraps replaced with proper error propagation.

Missing:
- No BlobIndex mutation (encrypted blobs not deleted from backends)
- No blob garbage collection (deferred to Tier 4)

### Defrag Blobs (`src/cli/defrag_blobs.rs`): STUB

68 lines total. Loads blob index, iterates paths, sums sizes, then does
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

`BluError` in `src/error.rs` has 23 variants (added `BlockHashMismatch`),
9 From impls, covers all major error categories. Well-structured with
thiserror. All 24 bare `.unwrap()` calls in production CLI code have
been replaced with proper error propagation. The joke `assert_eq!`
panic in encrypt_files has been replaced with `BlockHashMismatch`.

### Test Coverage: GAPS IN CLI

195 tests passing, heavily concentrated in crypto/keys/agent/format
modules. Zero tests for any CLI subcommand: status, delete, search,
defrag, sync, encrypt, restore, list, backend. The entire user-facing
surface is untested.

## Bare Unwraps in Production Code

All 24 bare `.unwrap()` calls (13 CLI + 11 core lib) have been replaced
with proper `BluError` propagation. The `assert_eq!` panic in
encrypt_files.rs was replaced with `BluError::BlockHashMismatch`.

## Build Notes

- S3 dependency (aws-config, aws-sdk-s3) always compiled in; should be
  feature-gated (noted in Cargo.toml line 32)
- security-framework (macOS Touch ID) unconditionally compiled; should
  be feature-gated for Linux builds
- No CI, no changelog, no release process
