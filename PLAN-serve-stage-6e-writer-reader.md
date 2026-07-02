# blu serve Stage 6e: v3 writer and reader implementation plan

Static design document for implementing the v3 segmented AEAD writer
and reader, refining Stage 6e of
`PLAN-serve-stage-6-segmented-aead.md` with two locked decisions from
implementation review.

Progress is recorded by git history and the working conversation, not
by mutating this file.

## Locked decisions

- `encrypt_envelope_segmented` returns `Result<Vec<u8>>` (no vestigial
  `Vec<SegmentSpan>`). Compression and per-chunk compressed-end
  tracking are a separate step in `compression.rs`, keeping the
  `dek_provider` crypto seam free of chunk-boundary semantics. This
  mirrors the existing v2 path (`compress()` then `encrypt_envelope()`).
- The real prefix cache is built in Stage 6e, fed by a whole-blob
  `read_data` for now. Stage 6f swaps only the fetch mechanism
  (whole-blob read becomes a header range-read plus a segment-prefix
  range-read); the decrypt, decompress, cache, and slice logic is
  written once here and is not rewritten later. No build-twice.

## Stage 6e.1: compression with per-region progress

6e.1a: Add `compress_with_progress(data: &[u8], region_endpoints:
     &[usize]) -> io::Result<(Vec<u8>, Vec<u64>)>` in
     `src/compression.rs`. Wrap `flate2::write::GzEncoder<Vec<u8>>`;
     for each region write its bytes then `flush()` (Z_SYNC_FLUSH
     preserves the LZ77 dictionary, keeping cross-chunk context).

6e.1b: Record `compressed_ends[i] = encoder.get_ref().len()` after each
     flush; `finish()` to emit the gzip trailer and return the full
     compressed stream plus the per-region compressed-end vector.

6e.1c: Tests: `compressed_ends` is monotonically increasing; the full
     stream decompresses back to the input; a single region equals a
     whole-blob compress followed by a flush.

## Stage 6e.2: segmented envelope writer

6e.2a: Add `encrypt_envelope_segmented(compressed: &[u8], segment_size:
     usize, keys: &DekProvider) -> Result<Vec<u8>>` in
     `src/dek_provider.rs`. `wrap_dek()`, zero-pad `compressed` up to a
     `segment_size` multiple, `segment_count = ceil(len / segment_size)`.

6e.2b: For each segment `i`, `dek.encrypt_segment(i, slice)`;
     concatenate the `ciphertext || tag` records.

6e.2c: Assemble via `v3format::write_v3` with `plaintext_len =
     compressed.len()` (the pre-pad length).

6e.2d: Test: round-trips with the 6e.3 decrypt path.

## Stage 6e.3: segmented prefix reader

6e.3a: Add `decrypt_envelope_segmented_prefix(data: &[u8], up_to_seg:
     u32, keys: &DekProvider) -> Result<Vec<u8>>` in
     `src/dek_provider.rs`. `v3format::read_header`, `unwrap_dek`,
     decrypt segments `0..=up_to_seg` (each `segment_size + 16` bytes on
     disk), concatenate the plaintext.

6e.3b: If `up_to_seg == segment_count - 1`: full stream. Decompress and
     let the gzip trailer terminate (padding is post-trailer and
     ignored).

6e.3c: Else prefix: decompress via a `read()` loop, treating
     `UnexpectedEof` as "stop, return the bytes decoded so far". This is
     the core prefix-fetch capability.

6e.3d: Tests: a front-segment prefix yields the correct leading bytes;
     a full read equals the `compress_with_progress` input;
     wrong-key/tamper fails.

## Stage 6e.4: version-dispatched blob reader with prefix cache

6e.4a: Cache becomes `LruCache<Hash, (Vec<u8>, usize)>` = (decompressed
     bytes, covered decompressed length). The v2 path sets covered
     length equal to the full decompressed length.

6e.4b: Dispatch on `v3format::peek_version`: v2 keeps the existing
     whole-blob decrypt path; v3 fetches the whole blob via `read_data`
     (Stage 6f swaps to a range read), reads the header, computes
     `up_to_seg = compressed_end / segment_size`, and calls
     `decrypt_envelope_segmented_prefix`.

6e.4c: Serve from cache when `pos.offset + pos.size <= covered_len`;
     otherwise fetch/extend and keep the longest prefix. A v3 chunk
     whose `compressed_end` is `None` is a hard error (index/blob format
     mismatch).

## Stage 6e.5: route blob writes through the segmented path

6e.5a: Add `DEFAULT_SEGMENT_SIZE: usize = 524_288` (512 KiB) constant in
     `src/blob.rs`.

6e.5b: In `BlobBuffer::seal_and_upload`, sort `self.positions` by
     `offset`, derive per-chunk `region_endpoints`, call
     `compress_with_progress`, then `encrypt_envelope_segmented(&compressed,
     DEFAULT_SEGMENT_SIZE, ..)`.

6e.5c: Set each `BlobBlockLocation.compressed_end = Some(compressed_ends[i])`
     via `new_v3`; hashing, path derivation, and upload are unchanged.
     Index writes via `gen_std_enc_serde!` stay v2.

## Stage 6e.6: tests

6e.6a: Write a multi-chunk blob through `BlobBuffer` (now v3), read every
     chunk back via `EncBlobReader::get_bytes` byte-for-byte; assert
     `compressed_end` is `Some` and monotonic.

6e.6b: Assert a front chunk decrypts strictly fewer segments than a tail
     chunk (instrument a test-only `decrypt_segment` counter; the
     byte-level fetch-cost assertion lands in Stage 6g once `read_range`
     exists).

6e.6c: Manually write a v2 `FileType::Blob` into a Local backend and
     confirm the reader's v2 branch still returns it correctly.

## Verification

After each sub-stage: `cargo test`, `cargo clippy`, `cargo fmt -- --check`.
