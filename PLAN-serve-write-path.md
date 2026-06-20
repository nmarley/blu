# blu serve Stage 5: write path implementation plan

Static design document for implementing `PutObject`, `DeleteObject`,
multipart upload, and the index flush strategy for `blu serve`, per
`BLU_SERVE_DESIGN.md` section 6 and `PLAN-blu-serve.md` Stage 5.

Progress is recorded by git history and the working conversation, not
by mutating this file.

## Codebase findings

`BlobBuffer::add_chunk` already takes `&mut [u8]`, not a file path
(`src/blob.rs:77`). It feeds bytes into the buffer, hashes the chunk,
records the position, and seals + uploads when the buffer is full.
Stage 5a in `PLAN-blu-serve.md` says "adapt to accept byte streams"
but the adaptation is already done; only the chunking source needs
changing.

`Config::push_indexes` and `Config::write_*_index` exist for index
flush (`src/config.rs:335`, `src/config.rs:282-284`). The flush
strategy (5d) reuses these directly.

The delete cascade pattern exists in `src/cli/delete_files.rs:81-117`:
remove file from plain index, decrement block references, delete
fully-unreferenced chunks from blob index, drain dead blob paths and
delete from backend.

`ServeState` (`src/serve/server.rs:38`) currently holds `redb`,
`bucket_name`, `index_updated_at`, and `blob_reader`. It has no `cfg`,
`keys`, `backend`, or write mutex. The write path needs all four.

`RedbStore` (`src/serve/redb_store.rs`) has four tables (path, file,
blob, tag) and `populate_from_indexes` but no block_index table and no
dump-to-indexes method. Both are needed for Stage 5.

`Chunkerator` (`src/block/chunkerator.rs`) wraps `BufReader<File>`.
The serve write path receives bytes over HTTP, not from a file, so an
in-memory chunking function is needed.

## Design decisions (locked from trade-off review)

- In-memory `chunk_bytes` function, no Chunkerator refactor
- Add `block_index` table to redb for O(1) delete cascade
- Serialize all writes behind a `tokio::sync::Mutex`
- Include multipart upload in Stage 5

## Stage 5a: In-memory chunking and block_index table

5a.1: Add `chunk_bytes(data: &[u8], chunk_size: usize) -> Vec<Vec<u8>>`
     to `src/block/chunkerator.rs`. Splits a byte slice into
     chunk_size pieces (last chunk may be smaller). No change to the
     existing file-based `Chunkerator`.

5a.2: Add `BLOCK_INDEX` table definition to `redb_store.rs`:
     `chunk_hash bytes -> BlockRef CBOR`. Maps onto
     `PlainIndex::blocks` (chunk_hash -> file_hash -> Position).

5a.3: Update `RedbStore::populate_from_indexes` to also populate
     block_index from `PlainIndex::blocks_map_ref()`.

5a.4: Add `RedbStore` methods for block_index CRUD: `get_blockref`,
     `insert_blockref`, `delete_blockref`. BlockRef stores a
     `HashMap<Hash, Position>` of file_hash -> position; updates must
     merge or remove entries, not blind-replace.

5a.5: Add `RedbStore::dump_to_indexes() -> (PlainIndex, BlobIndex,
     TagIndex)`. Reverse of `populate_from_indexes`: reads all four
     tables back into the in-memory index structs. Needed by the flush
     strategy (5d) to serialize redb state to encrypted CBOR. Constructs
     a fresh `PlainIndex` with `new_empty()` then fills `files` and
     `blocks` from redb, sets `updated_at` to now.

5a.6: Add `RedbStore::block_count()` for the health endpoint.

## Stage 5b: PutObject handler

5b.1: Extend `ServeState` with four fields:
     `cfg: Config`, `keys: DekProvider`, `backend: BackendKind`,
     `write_mutex: tokio::sync::Mutex<()>`.

5b.2: Update `index_sync::sync_from_backend` to return `cfg`, `keys`,
     `backend` alongside the existing `(RedbStore, NaiveDateTime,
     EncBlobReader)` so the server can construct the full `ServeState`.

5b.3: Add `PUT /{bucket}/{*key}` route and `put_object_handler` to
     `server.rs`. The handler acquires the write mutex, then:

5b.4: Collect the request body into `Bytes`. Chunk via
     `chunk_bytes(&body, DEFAULT_CHUNK_SIZE)`. Hash each chunk
     (`ChunkMeta::new`) and the whole file (SHA-512 multihash). For
     each chunk, check `redb.get_blob_location(chunk_hash)`; skip
     chunks already in the blob index (dedup). Feed new chunks into a
     `BlobBuffer::new(&backend, keys.clone())`, then `finalize`. Update
     redb in a single write transaction: insert FileRef into
     file_index, insert path -> file_hash into path_index, insert
     chunk -> BlobBlockLocation entries into blob_index, insert/update
     chunk -> BlockRef entries into block_index.

5b.5: Return 200 with `ETag` header (file hash in double quotes).

5b.6: Handle overwrite: if the path already exists in path_index,
     remove the old file_hash entry and run the delete cascade on it
     before inserting the new one. This is the same semantics as S3
     (PUT to an existing key overwrites).

## Stage 5c: DeleteObject handler

5c.1: Add `DELETE /{bucket}/{*key}` route and `delete_object_handler`
     to `server.rs`. The handler acquires the write mutex, then:

5c.2: Resolve path -> file_hash -> FileRef via redb. Return 404 if
     the path does not exist.

5c.3: For each chunk in the FileRef: look up BlockRef in block_index,
     remove this file_hash from its references. If the BlockRef is now
     empty, delete the chunk from blob_index (which marks the blob for
     deletion if it was the last live chunk). Delete the BlockRef from
     block_index.

5c.4: Drain `blob_index.paths_to_delete` (blobs with no live chunks)
     and delete them from the backend via `backend.delete(blob_path)`.

5c.5: Remove from redb: the path_index entry, the file_index entry,
     all block_index entries for fully-unreferenced chunks, and all
     tag_index entries for this file (scan tag_index for sets
     containing this file_hash, remove it, drop empty tag entries).

5c.6: Return 204 No Content.

## Stage 5d: Index flush strategy

5d.1: Implement a debounced flush. After each successful write
     (PutObject or DeleteObject), reset a debounce timer. When the
     timer fires (default 5 seconds after the last write), dump redb
     state to indexes via `dump_to_indexes`, write them locally via
     `Config::write_*_index`, and push to backend via
     `Config::push_indexes`.

5d.2: Store the debounce timer as
     `flush_timer: tokio::sync::Mutex<Option<JoinHandle<()>>>` in
     `ServeState`. On each write, lock the mutex, abort the existing
     timer if present, spawn a new one that sleeps for the debounce
     interval then runs the flush.

5d.3: Flush on graceful shutdown. In `serve()`, after
     `axum::serve(...).await` returns (shutdown signal received), do a
     final synchronous flush so no writes are lost. This is a
     best-effort flush; the debounce timer is cancelled and the flush
     runs inline.

5d.4: The flush acquires the write mutex so it does not race with
     in-flight writes. It reads redb (no write transaction needed for
     reading), constructs the index structs, writes them to local disk
     and pushes to backend.

## Stage 5e: Multipart upload

5e.1: Define `MultipartState` in `server.rs`:
     `path: String`, `parts: Vec<Vec<u8>>`, `created_at:
     NaiveDateTime`. Stored in an in-memory map keyed by upload_id.

5e.2: Add `multipart_uploads: tokio::sync::Mutex<HashMap<String,
     MultipartState>>` to `ServeState`. This is separate from the
     write mutex because part uploads do not touch redb or the blob
     pipeline until completion.

5e.3: `POST /{bucket}/{*key}?uploads` -> CreateMultipartUpload.
     Generate an upload_id (random 16-byte hex string). Insert
     `MultipartState { path, parts: vec![], created_at: now() }` into
     the map. Return XML:
     `<InitiateMultipartUploadResult><Bucket>...</Bucket><Key>...</Key><UploadId>...</UploadId></InitiateMultipartUploadResult>`.

5e.4: `PUT /{bucket}/{*key}?partNumber=N&uploadId=X` -> UploadPart.
     Look up the multipart state by upload_id. Append the request body
     bytes to `parts[N-1]` (1-indexed part numbers, store as
     0-indexed). Return 200 with an ETag header (hash of the part
     bytes). If the upload_id is not found, return 404
     NoSuchUpload.

5e.5: `POST /{bucket}/{*key}?uploadId=X` -> CompleteMultipartUpload.
     Acquire the write mutex. Remove the multipart state. Concatenate
     all parts in order into a single byte vector. Run the full
     PutObject pipeline (chunk, hash, dedup, pack, encrypt, upload,
     update redb). Return XML:
     `<CompleteMultipartUploadResult><Location>...</Location><Bucket>...</Bucket><Key>...</Key><ETag>...</ETag></CompleteMultipartUploadResult>`.

5e.6: `DELETE /{bucket}/{*key}?uploadId=X` -> AbortMultipartUpload.
     Remove the multipart state. Return 204. Discard all buffered part
     data.

## Stage 5f: End-to-end tests

5f.1: Test PutObject then GetObject round-trip: PUT a file via the
     router, GET it back, assert byte-for-byte equality. Verify redb
     now has the path, file, blob, and block entries.

5f.2: Test DeleteObject: PUT a file, DELETE it, assert 404 on
     subsequent GET and HEAD. Verify redb entries are gone. If the
     file was the only consumer of its chunks, verify the blob is
     deleted from the backend.

5f.3: Test dedup: PUT the same content to two different paths. Verify
     redb blob_index count does not increase on the second PUT (chunks
     are reused). Verify both paths resolve to the same file_hash.

5f.4: Test overwrite: PUT a file to a path, PUT different content to
     the same path. Verify GET returns the new content. Verify the old
     chunks are cleaned up if no other file references them.

5f.5: Test multipart upload: CreateMultipartUpload, UploadPart x3,
     CompleteMultipartUpload, then GET the object and verify the
     concatenation of all parts. Test AbortMultipartUpload cleans up
     state.

5f.6: Test flush: after a PUT, trigger the debounce timer (short
     interval in tests), verify that the local index files are written
     to `idxdir` and the encrypted indexes are pushed to the backend
     (verify via `backend.read_from_path`).

5f.7: Manual test with `aws --endpoint-url http://localhost:7777 s3 cp
     file s3://bucket/path` and `aws s3 rm s3://bucket/path`. Verify
     the file appears in `aws s3 ls` and is retrievable with `aws s3
     cp s3://bucket/path -`.
