# blu serve hardening plan

Static design document for hardening `blu serve` after the initial
Phase 1-3 (Stage 6f) implementation landed. Addresses correctness
data-loss bugs, v3 crypto hardening, large-object usability, and
doc-vs-code drift found in the master review.

Decisions locked from the review:

- Content-defined chunking is aspirational; fix the docs to state
  fixed-size chunking (do not implement CDC in this pass)
- Bind the v3 header fields into segment AAD now (breaking format
  change; existing v3 blobs are invalidated, acceptable under
  greenfield rules)
- Document the plaintext `.blu/serve.redb` as an accepted local
  at-rest tradeoff (do not encrypt it in this pass)
- Scope: correctness + crypto + usability + doc truth-up. Defer
  benchmarks (Stage 6g), returning-machine delta sync, redb at-rest
  encryption, and a crash-atomic index-push WAL.

Progress is recorded by git history and the working conversation, not
by mutating this file.

## Stage 1: v3 crypto hardening (breaking format change)

1a. Change segment AAD in `src/keys/dek.rs` (`encrypt_segment` /
    `decrypt_segment`) to bind the header fields, not just the index:
    `aad = index_le || segment_size_le || segment_count_le ||
    plaintext_len_le`. Thread the header fields in via added params or
    a small `SegmentAad` struct.
1b. Update `encrypt_envelope_segmented` and
    `decrypt_envelope_segmented_prefix` in `src/dek_provider.rs` to
    pass the header fields into the AAD.
1c. Replace the panicking slice at `src/dek_provider.rs:300`
    (`&compressed[..plaintext_len]`) with a checked bound that returns
    `BluError::DecryptionFailed` when `plaintext_len > compressed.len()`.
1d. Tests: tampered `plaintext_len` / `segment_count` in the header
    fails authentication (not panic); oversized `plaintext_len`
    returns a clean error; existing round-trip / prefix / tamper /
    wrong-key tests still pass.
1e. Confirm no on-disk v3 fixtures need regenerating; `cargo test` +
    `cargo clippy` clean.

## Stage 2: Dedup-vs-delete data-loss fix

2a. In `src/serve/redb_store.rs` `delete_object_index`, stop deleting
    the shared `FileRef` and all its paths when only one key is
    removed. Remove a single path; only cascade-delete the `FileRef`,
    chunks, and blobs when the last path referencing that file_hash is
    gone.
2b. Update `delete_object_handler` (`src/serve/server.rs:1038`) and
    the overwrite cascade in `put_object_full`
    (`src/serve/server.rs:887`) to call the path-scoped delete.
2c. Replace corruption-swallowing `deserialize_cbor(...).ok()` /
    `unwrap_or_else(BlockRef::new)` at `redb_store.rs:413,466,500`
    with hard errors so corrupt refs never silently drop references.
2d. Tests: two keys same content, delete one, assert the other still
    resolves and its blob survives; delete last key drops the blob;
    corrupt-ref path errors.

## Stage 3: Non-atomic overwrite fix

3a. In `put_object_full` (`src/serve/server.rs:881`), reorder so new
    content is chunked, packed, uploaded, and redb-committed before
    the old `FileRef`'s now-unreferenced blobs are reclaimed. Compute
    old-vs-new blob sets and only delete blobs unreferenced after the
    new write commits.
3b. Tests: overwrite with new content deletes no old blob until after
    the new commit; overwrite with identical content does not
    delete-then-reupload the shared blob.

## Stage 4: Large-object usability (body limit + streaming)

4a. Add `DefaultBodyLimit::disable()` (or a large configurable cap)
    layer to the router in `serve()` (`src/serve/server.rs:153`) so
    real PUTs are not rejected at the 2 MB default.
4b. Convert `put_object_handler` from `body: Bytes` to a streaming
    `Body`, spooling to a temp file for large uploads before chunking
    to keep peak memory bounded.
4c. Convert non-range `GetObject` (`fetch_file_bytes`,
    `src/serve/server.rs:468`) to a streaming response body that
    fetches and emits chunk-by-chunk instead of one
    `Vec::with_capacity(total)`.
4d. Cap multipart memory: spool parts to temp files keyed by
    upload_id, and add a stale-upload reaper using
    `MultipartState::created_at` (`src/serve/server.rs:62`).
4e. Tests: large PUT round-trip above the old 2 MB limit; streaming
    GET of a multi-blob file; multipart spool round-trip; stale-upload
    reap.

## Stage 5: Robustness polish

5a. Wire the `EncBlobReader` cache-capacity knob through `ServeState`
    construction (config field / CLI flag), replacing the hardcoded
    `EncBlobReader::new` at `src/serve/index_sync.rs:86`.
5b. Fix `Last-Modified` to RFC 7231 HTTP-date format in the
    `head_object` / `get_object` handlers; add a test asserting the
    format.
5c. Add a bounded retry loop around `sync_from_backend` so a transient
    backend failure at startup does not wedge the daemon in permanent
    503.
5d. Replace `.expect("invalid bind address")`
    (`src/serve/server.rs:141`) and the signal-handler `.expect`
    (`:122`) with `BluError` returns.
5e. Tests: Last-Modified format; sync retry succeeds after a transient
    failure.

## Stage 6: Documentation truth-up (doc-only commits)

6a. `BLU_SERVE_DESIGN.md`: correct constraint #1 to state fixed-size
    chunking (note CDC as future work); fix section 4 "binary search"
    to "linear scan"; add an explicit note that `.blu/serve.redb` is
    plaintext at rest and local-disk-at-rest is out of scope (the
    mnemonic-only recovery guarantee applies to the backend); note in
    section 5/10 that v3 range-fetch exposes chunk byte-offsets to a
    request-observing provider (a tradeoff v2 whole-blob fetch
    avoids); document the new header-AAD binding in the v3 wire-format
    section.
6b. `BLU_SERVE_DESIGN.md` section 3: correct the "returning machine
    diffs deltas" claim to match reality (upsert-only re-populate;
    delta sync is future work), or mark it explicitly aspirational.
6c. `PLAN-blu-serve.md`: note that Stage 6g (benchmarks) and
    delta-sync remain open, deferred out of this pass.
