// TODO: prep for std removal from library if possible
// #![cfg_attr(not(test), no_std)]

use flate2::bufread::{GzDecoder, GzEncoder};
use flate2::Compression;
use std::io::{self, Read, Write};

// TODO: std is necessary for io::Read, unfortunately. Also std::io::Result has
// no `core` equivalent.

pub(crate) fn compress(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut gz = GzEncoder::new(data, Compression::fast());
    let mut buf = Vec::new();
    gz.read_to_end(&mut buf)?;
    Ok(buf)
}

pub(crate) fn decompress(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut gz = GzDecoder::new(data);
    let mut buf = Vec::new();
    gz.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Decompress a *prefix* of a gzip stream, returning as many bytes as
/// can be decoded.
///
/// The input is expected to be a truncated gzip stream (a compressed
/// prefix produced by [`compress_with_progress`] and cut at a segment
/// boundary). Because the stream has no trailer, the decoder reaches
/// the end of the available compressed bytes mid-member; that surfaces
/// as an `UnexpectedEof`, which is treated as "stop here and return
/// what was decoded so far" rather than an error. This is the core
/// prefix-fetch capability: a reader can recover the leading
/// decompressed bytes without the whole blob.
pub(crate) fn decompress_prefix(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut gz = GzDecoder::new(data);
    let mut buf = Vec::new();
    let mut scratch = [0u8; 8192];
    loop {
        match gz.read(&mut scratch) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&scratch[..n]),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }
    Ok(buf)
}

/// Compress `data` as a single gzip stream while recording the
/// compressed-stream length at each region boundary.
///
/// `region_endpoints` are cumulative *decompressed* byte offsets
/// marking where each region ends (e.g. chunk sizes `[3, 4, 8]` yield
/// endpoints `[3, 7, 15]`). After writing each region the encoder is
/// flushed with `Z_SYNC_FLUSH`, which emits all bytes buffered so far
/// while preserving the LZ77 dictionary so cross-region compression
/// context is retained. The compressed length after each flush is
/// recorded, giving the reader the compressed offset where each
/// region's bytes end.
///
/// Returns the full gzip stream (including the trailer) plus a vector
/// of per-region compressed-end offsets, one per entry in
/// `region_endpoints`.
pub(crate) fn compress_with_progress(
    data: &[u8],
    region_endpoints: &[usize],
) -> io::Result<(Vec<u8>, Vec<u64>)> {
    use flate2::write::GzEncoder as WriteGzEncoder;

    let mut encoder = WriteGzEncoder::new(Vec::new(), Compression::fast());
    let mut compressed_ends = Vec::with_capacity(region_endpoints.len());

    let mut prev = 0usize;
    for &end in region_endpoints {
        // Region endpoints must be non-decreasing and within bounds.
        debug_assert!(end >= prev, "region endpoints must be non-decreasing");
        debug_assert!(end <= data.len(), "region endpoint out of bounds");
        encoder.write_all(&data[prev..end])?;
        // Z_SYNC_FLUSH: emit buffered output, keep the dictionary.
        encoder.flush()?;
        compressed_ends.push(encoder.get_ref().len() as u64);
        prev = end;
    }

    let compressed = encoder.finish()?;
    Ok((compressed, compressed_ends))
}

#[cfg(test)]
mod test {
    use super::{compress, compress_with_progress, decompress};
    use std::path::Path;

    const TEST_BLOCKS_DIR_T1: &str = "test/blocks/t1/";

    #[test]
    fn compress_decompress() {
        let path = Path::new(TEST_BLOCKS_DIR_T1).join("file1.txt");
        let data = std::fs::read(path).unwrap();
        // dbg!(data.len());

        let compressed = compress(&data).unwrap();
        // dbg!(compressed.len());

        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(data, decompressed);
    }

    #[test]
    fn compress_with_progress_monotonic_and_round_trips() {
        // Build a multi-region payload with real, compressible content.
        let mut data = Vec::new();
        let region_sizes = [1000usize, 2000, 500, 3000];
        for (i, &sz) in region_sizes.iter().enumerate() {
            data.extend(std::iter::repeat_n(b'a' + i as u8, sz));
        }
        let mut endpoints = Vec::new();
        let mut acc = 0;
        for &sz in &region_sizes {
            acc += sz;
            endpoints.push(acc);
        }

        let (compressed, ends) = compress_with_progress(&data, &endpoints).unwrap();

        // One compressed-end per region.
        assert_eq!(ends.len(), region_sizes.len());

        // Compressed ends are monotonically non-decreasing.
        for w in ends.windows(2) {
            assert!(w[1] >= w[0], "compressed ends must be non-decreasing");
        }

        // The final compressed-end is <= the full stream length (the
        // gzip trailer is appended by finish() after the last flush).
        assert!(*ends.last().unwrap() <= compressed.len() as u64);

        // The full stream decompresses back to the original input.
        let round_tripped = decompress(&compressed).unwrap();
        assert_eq!(round_tripped, data);
    }

    #[test]
    fn compress_with_progress_single_region_matches_flush() {
        let data = vec![0x42u8; 4096];
        let endpoints = [data.len()];

        let (compressed, ends) = compress_with_progress(&data, &endpoints).unwrap();
        assert_eq!(ends.len(), 1);

        // A single region's compressed-end is the length after the sole
        // flush, before the trailer is written by finish().
        assert!(ends[0] <= compressed.len() as u64);

        let round_tripped = decompress(&compressed).unwrap();
        assert_eq!(round_tripped, data);
    }

    #[test]
    fn compress_with_progress_empty_input() {
        let (compressed, ends) = compress_with_progress(&[], &[]).unwrap();
        assert!(ends.is_empty());
        let round_tripped = decompress(&compressed).unwrap();
        assert!(round_tripped.is_empty());
    }
}
