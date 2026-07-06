# Start Here

Last updated: 2026-07-05

Version: 0.5.0 (pre-release, beta quality)

## Overall Assessment

The cryptographic core (envelope encryption, PQ hybrid KEK wrapping,
ChaCha20-Poly1305 pipeline, agent daemon with mlock'd memory) is solid
and well-tested. The content-addressed storage model, named multi-backend
config system, and backend mirror/diff commands are polished. The CLI
surface layer has improved: delete_files has a full cascade with inline
scrub support, defrag-blobs is a real repack command backed by shared
logic, bare unwraps have been replaced with proper error propagation
(24 fixed), and dead config code has been removed. Tier 1 (data
pipeline) is complete. Zero CLI-layer tests still exist.

Since the last pulse, v3 segmented AEAD blobs landed (fixed-size
segments with counter-derived nonces, prefix-fetch reads, v2 upgrade
path), and `blu serve` is a working local S3-compatible daemon (redb
index, multipart upload, debounced flush, graceful shutdown, traffic
countermeasures). Index serde migrated from bincode to ciborium. Scrypt
work factor is pinned to a minimum of 18.

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

### v3 Segmented AEAD Blob Format: NEW

v3 replaces v2's single sealed AEAD box with fixed-size, independently
authenticated segments. Each segment uses the blob's DEK with a
counter-derived nonce (12 bytes: [0x00;4] || u64 LE segment index) and
the segment index as AAD. This enables prefix-fetch reads: a reader can
download and decrypt only the segments covering a chunk's compressed
bytes without fetching the whole blob.

Index files (`BLUI`) remain v2; they are always read whole and gain
nothing from segmentation.

The v2 → v3 upgrade path is wired through `defrag-blobs --upgrade-format`
(or inline via `delete-files --scrub` on a v2 vault). `EncBlobReader`
caches the longest decompressed prefix per blob to avoid re-fetching
segments.

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
blobs (still have live chunks, tracked in `paths_to_repack` for defrag).
`--scrub` flag triggers inline repack of partially-dead blobs after
deletion. Without `--scrub`, prints an advisory with the count of
blobs pending repack. Tests cover partial deletion, full deletion,
drain semantics, error cases, multi-blob scenarios, end-to-end backend
file removal, repack round-trips, and data integrity verification.

### Defrag Blobs (`src/cli/defrag_blobs.rs`): COMPLETE

Fully rewritten. Loads blob index from vault config (like other
commands), checks `BlobIndex::paths_to_repack` for candidates, and
calls the shared `repack_blobs()` function. Supports `--dry-run`
(lists candidates with live chunk counts) and `--backend` for
targeting a specific named backend. The shared repack logic reads
live chunks from old blobs via `EncBlobReader`, writes them into
fresh `BlobBuffer` instances (re-compressed, re-encrypted with new
DEKs), deletes old blobs from the backend, and returns stats.

`delete-files --scrub` calls the same `repack_blobs()` for inline
repack after deletion. Without `--scrub`, an advisory message shows
how many blobs have dead chunks pending repack.

`--upgrade-format` upgrades v2 blobs to v3 during repack.

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
The blob module has tests covering BlobIndex mutation, partial/full
blob death, drain semantics, end-to-end backend file removal, repack
round-trips (surviving chunks in new blobs, old blobs gone), noop
repack, and data integrity verification after repack. Zero tests for
CLI subcommand entry points: status, delete, search, defrag, sync,
encrypt, restore, list, backend.

### `blu serve` Local Daemon: WORKING

A translation layer that presents decrypted, de-obfuscated files to any
S3-compatible client while the real backend holds only opaque,
content-addressed encrypted blobs. Uses axum for HTTP, redb for the
local index store, and supports:

- `GetObject` and `HeadObject` (read path)
- `ListObjectsV2` (prefix listing)
- `PutObject` and multipart upload (write path)
- `DeleteObject`
- Startup index sync from backend
- Debounced index flush after vault changes
- Graceful shutdown
- Traffic analysis countermeasures (decoy padding, timing jitter)

Missing: full end-to-end write path tests, streaming index I/O.

## Bare Unwraps in Production Code

All bare `.unwrap()` calls in production code have been replaced
with proper `BluError` propagation.

## Build Notes

- S3 dependency (aws-config, aws-sdk-s3) always compiled in; should be
  feature-gated (noted in Cargo.toml line 32)
- security-framework (macOS Touch ID) unconditionally compiled; should
  be feature-gated for Linux builds
- No CI, no changelog, no release process
