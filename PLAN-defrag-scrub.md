# Plan: Blob Defrag and Delete --scrub

## Summary

Add a `paths_to_repack` field to `BlobIndex` so partially-dead blobs
are tracked deterministically. Wire this into `delete-files --scrub`
for inline repack and rewrite `defrag-blobs` as the standalone batch
equivalent. No heuristics; every repack candidate is known exactly.

## Design

`BlobIndex` gains a new persisted field:

```
paths_to_repack: HashSet<PathBuf>
```

`delete_chunk()` populates it when a chunk is removed but the blob
still has live chunks. `paths_to_delete` continues to be populated
only when the last chunk in a blob is removed. Both sets persist
across operations via the existing `EncryptedSerializable` pipeline.

Shared repack logic (used by both `--scrub` and `defrag-blobs`):

1. For each blob path in `paths_to_repack`:
   a. Read blob from backend via `EncBlobReader::get_bytes()` for
      each live chunk (positions from `path_index`).
   b. Feed live chunks into a fresh `BlobBuffer` via `add_chunk()`.
      This re-compresses, re-encrypts with a new DEK, and uploads.
      `BlobIndex.map` entries update automatically (insert overwrites).
   c. Remove old blob path from `path_index`.
2. After all candidates are repacked, `BlobBuffer::finalize()` flushes
   the last blob.
3. Delete old blob files from backend.
4. Clear `paths_to_repack`.
5. Write updated `BlobIndex`.

## Stages

Stage 1: Add `paths_to_repack` to `BlobIndex` [DONE]
  1a. Add the field to the struct with serde, default empty
  1b. Update `delete_chunk()` to populate it for partial deletes
  1c. Add `drain_paths_to_repack()` method
  1d. Tests: verify partial delete populates `paths_to_repack`,
      full delete does not, drain clears the set

Stage 2: Shared repack function [DONE]
  2a. Add `repack_blobs()` in `src/blob.rs`. Takes `&mut BlobIndex`,
      `&BackendKind`, `&DekProvider`. Operates on `paths_to_repack`.
  2b. Read each live chunk from old blob via `EncBlobReader`
  2c. Write into fresh `BlobBuffer`, finalize, delete old blobs
  2d. Clear `paths_to_repack`
  2e. Return stats (blobs repacked, chunks moved, old blobs deleted)
  2f. Tests: repack round-trip, noop, data integrity verification

Stage 3: Wire `--scrub` into `delete-files` [DONE]
  3a. Add `--scrub` flag to `DeleteFilesArgs`
  3b. After existing delete cascade, if `--scrub`: call `repack_blobs()`
  3c. Print scrub stats
  3d. If not `--scrub` and `paths_to_repack` is non-empty, print
      advisory message with count

Stage 4: Rewrite `defrag-blobs` [DONE]
  4a. Drop raw `blob_index_path` arg, load from config like other cmds
  4b. Make async, add `--backend` flag
  4c. Call `repack_blobs()` on accumulated `paths_to_repack`
  4d. Dry-run mode: print what would be repacked without doing it
  4e. Print stats, write updated blob index

Stage 5: Update docs and metadata [DONE]
  5a. Update PRIOS.md (mark item 8 done)
  5b. Update PULSE.md defrag section
