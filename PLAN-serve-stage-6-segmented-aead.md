# blu serve Stage 6: segmented AEAD (v3 format) implementation plan

Static design document for implementing the v3 blob format (fixed-size
segmented AEAD) and its byte-range read path, per `BLU_SERVE_DESIGN.md`
section 5 and `PLAN-blu-serve.md` Stage 6.

Progress is recorded by git history and the working conversation, not
by mutating this file.

## Why

v2 seals each blob as a single ChaCha20-Poly1305 box: `compress(chunk1
|| ... || chunkN) -> encrypt_as_one_unit`. The Poly1305 tag covers the
whole ciphertext, so a reader must download and decrypt the entire
~64 MiB blob to recover a single 512 KiB chunk. For `blu serve`
streaming (video seek, partial reads) every cache miss costs a full
blob fetch. v3 splits the compressed payload into fixed-size,
independently authenticated segments so the reader can fetch and
decrypt a bounded prefix instead of the whole blob, while keeping blobs
uniform-sized and structureless to the storage provider (no metadata
leakage).

## Codebase findings

`src/v2format.rs` defines the on-disk envelope: magic (`BLUB`/`BLUI`),
`format_version: u16` (currently `2`), `kek_version: u16`, wrapped-DEK
length + bytes, then the DEK-encrypted payload as one AEAD box.
`read_header` already validates the version and rejects anything that
is not `FORMAT_VERSION`. Dispatch on version happens here.

`src/keys/dek.rs` wraps ChaCha20-Poly1305 with a *random* 12-byte nonce
per `encrypt_data` call (`NONCE_SIZE = 12`, `TAG_SIZE = 16`). Segmented
encryption needs a *deterministic, counter-derived* nonce per segment,
so a new pair of methods is required alongside the existing
random-nonce ones. Each blob already gets a fresh DEK, so a per-segment
counter nonce is unique under that key.

`src/dek_provider.rs` `encrypt_envelope` / `decrypt_envelope` are the
free functions blobs use. `encrypt_envelope` calls `dek.encrypt_data`
(single box). v3 needs a parallel `encrypt_envelope_segmented` /
`decrypt_envelope_segmented` (or an internal branch) that emits/reads
the v3 layout.

`src/blob.rs` `BlobBuffer::seal_and_upload` (line 131) is the only blob
writer: `compress(&self.data)` -> `encrypt_envelope(..., FileType::Blob)`
-> hash the ciphertext -> `storage::path_for` -> update `BlobIndex`
`positions` with `Position { offset, size }`, where `offset`/`size` are
**decompressed** byte offsets into the blob. `EncBlobReader::get_bytes`
(line 245) does the reverse: `read_data` (whole blob) -> `decrypt_envelope`
-> `decompress` -> slice `[offset..offset+size]`, caching the whole
decompressed blob in an LRU keyed by blob hash.

`src/block/blockindex` / `src/blob.rs` `BlobBlockLocation` holds `path`
+ `position: Position`. `Position` (`src/io.rs:76`) is `{ offset,
size }` (decompressed). v3 needs an additional compressed-offset field
so the reader knows which segments cover a chunk. Existing serialized
indexes lack it, so the new field must be `#[serde(default)]`.

`src/storage.rs` `BackendKind` dispatches `read_data` / `write_data` /
`read_from_path` etc. over `Local` and `AmazonS3`. There is no
byte-range read; both backends read whole objects. `AmazonS3::read_data`
(line 72) uses `get_object()` with no `.range(...)`; the AWS SDK
supports `.range("bytes=A-B")`. `Local::read_data` (line 27) uses
`tokio::fs::read` (whole file); a range read needs seek + bounded read.

`src/serve/server.rs` `fetch_range_bytes` (line 493) already walks
`FileRef::chunkmetas`, accumulates decompressed offsets, and fetches
only the chunks overlapping an HTTP `Range`, via
`EncBlobReader::get_bytes` per chunk. Today each `get_bytes` still
pulls the whole blob on a cache miss. v3 makes that per-chunk fetch
cheap; the range handler above it needs no structural change.

`src/serve/redb_store.rs` `dump_to_indexes` / `populate_from_indexes`
round-trip `BlobBlockLocation` through the `blob_index` table. Adding a
field to `BlobBlockLocation` means both directions must carry it.

`src/cli/defrag_blobs.rs` + `repack_blobs` (`src/blob.rs:299`) already
read live chunks through `EncBlobReader` and rewrite them into a fresh
`BlobBuffer`. The `--upgrade-format` migration is a variant of this:
read v2 blobs, rewrite as v3. `DefragBlobsArgs`
(`src/cli/clapargs.rs:215`) currently has `dry_run` and `backend`.

There is no benchmark harness in the repo (no `criterion`, no
`benches/`). Stage 6g uses a deterministic *bytes-fetched* metric
(instrument the backend, not wall-clock) which is more meaningful and
CI-stable than timing.

## Design tension to resolve first (locked)

The design doc says two things that are in tension: compression is
"whole-blob ... preserving cross-chunk context" (section 5, line 281)
but cache-miss cost is "`N * SEGMENT_SIZE` where N is the number of
segments spanning the requested chunk" (line 273). With a single
whole-blob gzip stream you **cannot** decompress a middle segment
without every preceding compressed byte, so a true "fetch only N
spanning segments" is impossible under whole-blob gzip.

Locked decision: **whole-blob gzip, prefix fetch.** Keep one gzip
stream per blob (best ratio, preserves cross-chunk context, keeps
blobs uniform). Split the *compressed* output into fixed-size segments.
To recover a chunk whose compressed bytes end at offset `C`, the reader
fetches segments `0..=floor(C / SEGMENT_SIZE)` (a compressed prefix)
and decompresses that prefix. `BlobIndex` therefore stores, per chunk,
the compressed-end offset (decision 6d). This bounds fetch cost by the
chunk's position in the blob: front chunks are cheap, and because
`BlobBuffer` packs chunks in insertion order, sequential streaming (the
primary `blu serve` use case) reads front-to-back and each seek fetches
a minimal growing prefix that the LRU already holds. Worst case (a cold
seek to the last chunk) degrades to a whole-blob fetch, i.e. no worse
than v2. True per-chunk random access (segment-independent framing)
would require flushing the compressor at segment boundaries and is
recorded as explicit future work, not built here.

Corollary caching change: `EncBlobReader` currently caches the whole
decompressed blob keyed by blob hash. Under prefix fetch that still
works (a prefix decompress yields a prefix of the decompressed blob);
the cache stores the longest decompressed prefix seen so far per blob
and serves any chunk whose decompressed end is within it. Cache key
stays the blob hash; value gains the covered decompressed length.

## Design decisions (locked from trade-off review)

- Whole-blob gzip with compressed-prefix fetch (above); no
  segment-independent framing in this stage.
- Segment size default 512 KiB, stored in the v3 header (a per-blob
  value, not a global constant), so it is tunable without a format bump.
- Per-segment nonce = 4-byte fixed prefix (zero) || 8-byte little-endian
  segment counter. Unique because the DEK is unique per blob. The
  segment index is also passed as AEAD associated data so a segment
  cannot be moved to another index without failing authentication.
- v3 keeps the `BLUB` magic and the wrapped-DEK header; it bumps
  `format_version` to `3` and appends v3-specific header fields.
  `read_header` dispatches on version. v2 blobs remain readable forever.
- Once v3 ships, all newly written blobs are v3. No config flag, no
  dual-write. (Greenfield: no back-compat write path.)
- Migration is opt-in and offline via `blu defrag-blobs
  --upgrade-format`, reusing the repack machinery.

## Stage 6a: Write the v3 format spec into the design doc

6a.1: Before any code, extend `BLU_SERVE_DESIGN.md` section 5 with the
     concrete v3 wire format: header field order and widths (magic,
     `format_version = 3`, `kek_version`, wrapped-DEK len + bytes,
     `segment_size: u32`, `segment_count: u32`, `plaintext_len: u64`
     for the compressed-stream length before padding), the segment
     framing (`segment_count` records of exactly `12 + SEGMENT_SIZE +
     16` bytes each), the nonce construction (4-byte zero prefix ||
     8-byte LE counter), the AAD (segment index), and the padding rule
     (zero-pad the final compressed segment to `SEGMENT_SIZE`;
     `plaintext_len` lets the reader trim padding after decompression).

6a.2: Document the prefix-fetch read algorithm and the compressed-end
     offset stored per chunk, and record segment-independent framing as
     future work. This is a standalone documentation commit (no code),
     per the repo rule separating doc and code commits.

## Stage 6b: v3 format module (`src/v3format.rs`)

6b.1: Add `FORMAT_VERSION_V3: u16 = 3` and a `V3Header` struct
     (`kek_version`, `wrapped_dek`, `segment_size: u32`,
     `segment_count: u32`, `plaintext_len: u64`). Reuse `MAGIC_BLOB`.

6b.2: `write_v3_header` / `read_v3_header` mirroring the v2 helpers,
     with bounds checks and a truncation error path. Return the payload
     offset where segment 0 begins.

6b.3: `is_v3(data)` and a shared `peek_version(data) -> u16` so callers
     can branch v2 vs v3 without fully parsing. Keep `v2format` intact.

6b.4: Unit tests: header round-trip, truncated-header rejection,
     wrong-version rejection, `segment_count`/`segment_size` echoed
     back correctly.

## Stage 6c: Segment-aware DEK crypto

6c.1: Add `Dek::encrypt_segment(&self, index: u64, plaintext: &[u8]) ->
     Result<Vec<u8>>` and `Dek::decrypt_segment(&self, index: u64,
     ciphertext: &[u8]) -> Result<Vec<u8>>` in `src/keys/dek.rs`. Nonce
     = `[0u8;4] || index.to_le_bytes()`; pass `index` bytes as AAD via
     `Payload { msg, aad }`. Output is `ciphertext || tag` (no nonce
     stored inline; it is derived from the index).

6c.2: Unit tests: segment round-trip; wrong index (nonce/AAD mismatch)
     fails authentication; tampered ciphertext fails; two segments with
     the same plaintext but different indices produce different bytes.

## Stage 6d: BlobIndex compressed-offset field

6d.1: Add `compressed_end: Option<u64>` to `BlobBlockLocation`
     (`src/blob.rs:169`) with `#[serde(default)]`. `None` means a v2
     blob (whole-blob fetch). `Some(c)` means the chunk's compressed
     bytes end at offset `c` in the blob's compressed stream, so the
     reader fetches segments `0..=floor(c / segment_size)`.

6d.2: Update `BlobBlockLocation::new` and all constructors; update
     `redb_store.rs` `dump_to_indexes` / `populate_from_indexes` to
     carry the field through the `blob_index` table both ways.

6d.3: Confirm existing serialized indexes still deserialize (the
     `#[serde(default)]` makes old CBOR load with `compressed_end:
     None`). Add a test loading a v2-era `BlobBlockLocation` CBOR blob.

## Stage 6e: v3 writer and reader

6e.1: `encrypt_envelope_segmented(compressed: &[u8], segment_size:
     usize, keys: &DekProvider) -> Result<(Vec<u8>, Vec<SegmentSpan>)>`
     in `dek_provider.rs`: wrap a DEK, pad the compressed stream to a
     `segment_size` multiple, encrypt each segment with
     `encrypt_segment(i, ..)`, write the v3 header + segments. Return
     the file bytes plus per-input-region compressed spans so the
     writer can populate `compressed_end`.

6e.2: Teach `BlobBuffer::seal_and_upload` (`src/blob.rs:131`) to record
     each chunk's cumulative compressed-end offset. Because gzip is
     whole-blob, the per-chunk compressed offset is only known after
     compressing the full buffer; compute it by tracking cumulative
     decompressed sizes and mapping through the compressor, or (simpler
     and exact) compress incrementally and record the compressor output
     length after each chunk. Lock the simpler exact approach: feed the
     buffer to the compressor chunk-by-chunk with `flush` accounting so
     `compressed_end[i]` is the compressed length after chunk `i`.
     Populate `BlobBlockLocation.compressed_end` accordingly.

6e.3: `decrypt_envelope_segmented_prefix(header, raw_prefix, up_to_seg,
     keys)`: given a fetched prefix covering segments `0..=up_to_seg`,
     decrypt each segment, concatenate, decompress, return the
     decompressed prefix. Trim padding using `plaintext_len` only when
     the full blob is present.

6e.4: Branch `EncBlobReader::get_bytes` on version: v2 -> existing
     whole-blob path; v3 -> compute `up_to_seg` from the chunk's
     `compressed_end` and `segment_size`, range-fetch that segment
     prefix (Stage 6f), decrypt+decompress the prefix, slice
     `[offset..offset+size]`. Update the LRU to cache the longest
     decompressed prefix per blob hash and serve any chunk whose
     decompressed end falls within the cached length; extend on demand.

6e.5: Point `encrypt_envelope` blob writes at the segmented path so all
     new blobs are v3. Index files (`FileType::Index`) stay v2 (they
     are read whole; no range benefit). Keep `FileType` dispatch
     explicit.

6e.6: Round-trip tests: write a multi-segment v3 blob, read every chunk
     back byte-for-byte; verify a front chunk fetches only early
     segments and a tail chunk fetches the full prefix; verify v2 blobs
     still read after the writer switch.

## Stage 6f: Byte-range backend reads

6f.1: Add `BackendKind::read_range(&self, path: &Path, start: u64, end:
     u64) -> Result<Vec<u8>>` (end exclusive) dispatching to both
     backends.

6f.2: `Local::read_range`: open the file, `seek(SeekFrom::Start(start))`,
     read `end - start` bytes (tokio `AsyncSeekExt` + `take`). Clamp to
     EOF.

6f.3: `AmazonS3::read_range`: `get_object().range(format!("bytes={}-{}",
     start, end - 1))` (HTTP range is inclusive; convert). Collect the
     body.

6f.4: Wire `EncBlobReader` v3 path (6e.4) to `read_range` for the
     segment prefix `[0, (up_to_seg + 1) * on_disk_segment_len)` where
     `on_disk_segment_len = 12-nonce-free framing = segment_size + 16`
     plus the header offset. Compute offsets from the parsed `V3Header`.

6f.5: Tests: `Local` and (mocked or `#[ignore]` live) S3 range reads
     return the exact byte window; out-of-range clamps at EOF.

## Stage 6g: Migration and benchmarks

6g.1: Add `--upgrade-format` to `DefragBlobsArgs`
     (`src/cli/clapargs.rs:215`). When set, select all v2 blobs (scan
     `BlobIndex.path_index`, peek each blob's version via a small header
     read), read their live chunks through `EncBlobReader`, and rewrite
     them through a `BlobBuffer` (which now emits v3), then delete the
     old v2 blobs and push indexes. Reuse the `repack_blobs` structure;
     factor a shared `rewrite_blobs(candidates, ...)` helper if the
     overlap is clean.

6g.2: `--upgrade-format --dry-run` reports how many v2 blobs would be
     upgraded without writing.

6g.3: Deterministic fetch-cost test (in place of wall-clock benchmarks):
     instrument a counting wrapper over `BackendKind` (or count bytes in
     a `Local` temp dir read) and assert that reading a front chunk from
     a multi-segment v3 blob fetches strictly fewer backend bytes than
     the equivalent v2 whole-blob read. This is the concrete win Stage 6
     exists to deliver and is CI-stable.

6g.4: End-to-end serve test: PUT a large object (many segments), GET a
     `Range` covering an early byte window, assert byte-for-byte
     correctness and that fewer than the whole blob's bytes were fetched.

## Out of scope (recorded as future work)

- Segment-independent compression framing (flush the compressor at each
  segment boundary) for true per-chunk random access without a prefix
  fetch. Revisit if random-seek latency on cold caches proves
  unacceptable for large blobs.
- Switching the compressor from gzip to a seekable format (e.g. zstd
  seekable). Larger change, separate plan.
