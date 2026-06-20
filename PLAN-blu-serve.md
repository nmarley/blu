# blu serve implementation plan

Static design document for implementing `blu serve` per
`BLU_SERVE_DESIGN.md`. Decisions locked from the trade-off review:

- redb from day 1 (no in-memory adapter phase)
- axum for HTTP, revisit s3s at Phase 2 write support
- foreground `blu serve` subcommand (add `--detach` later)

Progress is recorded by git history and the working conversation, not
by mutating this file.

## Stage 1: Dependencies, skeleton, and doc corrections

1a. Add `redb` and `axum` to `Cargo.toml`
1b. Correct bogus claims in `BLU_SERVE_DESIGN.md` (axum is not a tokio
    transitive dep; `restore_files` does not use `EncBlobReader`; the
    storage seam is a `BackendKind` enum, not a `Backend` trait) and
    fix the stale `Backend` trait mention in `AGENTS.md`
1c. Create `src/serve/` module (`mod.rs`, `server.rs`,
    `redb_store.rs`, `index_sync.rs`)
1d. Add `blu serve` subcommand to `clapargs.rs` and dispatch in
    `src/bin/blu.rs` (foreground, listens on localhost:7777)
1e. `GET /_health` returns OK; verify `cargo build` + `cargo clippy`
    clean

## Stage 2: redb index store and startup sync

2a. Define redb table definitions (path -> file_hash,
    file_hash -> `FileRef` CBOR bytes, chunk_hash -> `BlobBlockLocation`
    CBOR bytes, tag -> `HashSet<Hash>` CBOR bytes)
2b. Implement `redb_store.rs`: open/create DB, bulk-insert from
    deserialized `PlainIndex` / `BlobIndex` / `TagIndex`
2c. Implement `index_sync.rs`: on startup call
    `cfg.pull_indexes(&backend)`, then existing `load_*_index` loaders,
    then populate redb; on subsequent starts, open existing redb then
    pull and diff deltas
2d. Tests for round-trip (insert from indexes, read back, compare)

## Stage 3: Read path, `ListObjectsV2`

3a. Wire redb path index into a virtual namespace query (prefix match
    on paths, pagination)
3b. axum handler translating `ListObjectsV2` XML request to a redb
    query, then to an S3 XML response
3c. Test with `aws --endpoint-url http://localhost:7777 s3 ls`

## Stage 4: Read path, `GetObject` + `HeadObject` with byte-range

4a. Wrap `EncBlobReader` in `Arc<tokio::sync::Mutex>`; make
    `BLOB_CACHE_CAPACITY` a constructor parameter (config knob,
    default 10). Clone the slice out of the cache under the lock, then
    release the lock and ship bytes so the cache lock stays short and
    no borrow crosses an await point
4b. Implement path -> file_hash -> `FileRef` -> ordered chunks ->
    `BlobBlockLocation` resolution against redb (reuse the algorithm
    from `src/cli/restore_files.rs:114-134`)
4c. `HeadObject`: compute total size from `FileRef::total_size()`,
    return headers
4d. `GetObject`: fetch, cache, slice, and serve via `EncBlobReader`;
    concatenate chunks in order into the response body
4e. `GetObject` with `Range`: binary search cumulative chunk offsets,
    fetch overlapping chunks, slice the requested byte range, return
    with `Content-Range`
4f. Test sequential read with `aws s3 cp` and range with
    `curl -H "Range: bytes=..."`; verify against a real vault

## Stage 5: Write path, `PutObject` + `DeleteObject`

5a. Adapt `BlobBuffer::add_chunk` / `seal_and_upload` to accept byte
    streams instead of file paths
5b. `PutObject`: buffer or spool incoming bytes, chunk via
    `Chunkerator`, hash, dedup against the redb `BlobIndex`, pack,
    encrypt, upload, update redb indexes
5c. `DeleteObject`: remove from redb indexes, trigger the delete
    cascade (reuse the `delete_files` cascade logic)
5d. Index flush strategy: periodic serialize redb state to encrypted
    CBOR and push to backend (debounced)
5e. Multipart upload (`CreateMultipartUpload`, `UploadPart`,
    `CompleteMultipartUpload`)
5f. End-to-end write plus read round-trip test

## Stage 6: Segmented AEAD (v3 format)

6a. Define v3 blob header (segment size stored in header, fixed-size
    segments, no in-blob table of contents)
6b. Specify the nonce construction explicitly
    (counter-derived, written into `BLU_SERVE_DESIGN.md` section 5
    before coding)
6c. Add `read_range(path, start..end)` to `BackendKind` (and to
    `Local` / `AmazonS3`) for byte-range S3 GET
6d. Add compressed-byte-offset field to `BlobIndex` entries so the
    client can compute which segments to fetch
6e. v3 writer and reader with v2 backward compatibility
6f. `blu defrag-blobs --upgrade-format` migration path
6g. Benchmarks: random-access latency v2 vs v3
