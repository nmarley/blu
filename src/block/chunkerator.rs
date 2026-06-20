use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::error::BluError;

/// Chunkerator reads files a "chunk" at a time, and returns chunks via the
/// iterator.
///
/// Example
/// ```rust
/// use blu::block::Chunkerator;
/// let chunker = Chunkerator::new("/etc/passwd", 512).unwrap();
/// for chunk in chunker {
///     println!("{:?}", chunk);
/// }
/// ```
#[derive(Debug)]
pub struct Chunkerator {
    buf_reader: BufReader<File>,
}

impl Chunkerator {
    /// Create a new Chunkerator, given a chunk size.
    pub fn new<P: AsRef<Path>>(filepath: P, chunk_size: usize) -> Result<Self, BluError> {
        let f = File::open(filepath.as_ref())?;
        let reader = BufReader::with_capacity(chunk_size, f);
        Ok(Self { buf_reader: reader })
    }
}

impl std::iter::Iterator for Chunkerator {
    type Item = Vec<u8>;
    fn next(&mut self) -> Option<Self::Item> {
        // fill entire reader
        let data = match self.buf_reader.fill_buf() {
            Ok(data) => data,
            Err(e) => {
                error!("Chunkerator read error: {}", e);
                return None;
            }
        };
        // handle None case (no more data to read)
        if data.is_empty() {
            return None;
        }
        let data = data.to_vec();
        self.buf_reader.consume(data.len());
        Some(data)
    }
}

/// Split an in-memory byte slice into chunks of `chunk_size` bytes.
///
/// The last chunk may be smaller than `chunk_size` if the input is
/// not evenly divisible. Returns an empty `Vec` if `data` is empty.
///
/// This is the in-memory counterpart to [`Chunkerator`], used by the
/// `blu serve` write path where bytes arrive over HTTP rather than
/// from a file.
pub fn chunk_bytes(data: &[u8], chunk_size: usize) -> Vec<Vec<u8>> {
    if data.is_empty() || chunk_size == 0 {
        return Vec::new();
    }
    data.chunks(chunk_size).map(|c| c.to_vec()).collect()
}

#[cfg(test)]
mod test {
    use super::{chunk_bytes, Chunkerator};
    use std::path::Path;

    const TEST_BLOCKS_DIR_T1: &str = "test/blocks/t1/";

    #[test]
    fn chunkerator() {
        let file5_path = Path::new(TEST_BLOCKS_DIR_T1).join("file5.txt");
        let mut chunker = Chunkerator::new(file5_path, 512).unwrap();
        let chunk = chunker.next();
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().len(), 512);
    }

    #[test]
    fn chunk_bytes_even_split() {
        let data = vec![0xAB; 1024];
        let chunks = chunk_bytes(&data, 256);
        assert_eq!(chunks.len(), 4);
        for chunk in &chunks {
            assert_eq!(chunk.len(), 256);
        }
        // Verify reassembly
        let reassembled: Vec<u8> = chunks.concat();
        assert_eq!(reassembled, data);
    }

    #[test]
    fn chunk_bytes_uneven_split() {
        let data = vec![0xCD; 1000];
        let chunks = chunk_bytes(&data, 256);
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].len(), 256);
        assert_eq!(chunks[1].len(), 256);
        assert_eq!(chunks[2].len(), 256);
        assert_eq!(chunks[3].len(), 232);
        let reassembled: Vec<u8> = chunks.concat();
        assert_eq!(reassembled, data);
    }

    #[test]
    fn chunk_bytes_single_chunk() {
        let data = vec![0x01, 0x02, 0x03];
        let chunks = chunk_bytes(&data, 4096);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], data);
    }

    #[test]
    fn chunk_bytes_empty_input() {
        let data: Vec<u8> = vec![];
        let chunks = chunk_bytes(&data, 4096);
        assert!(chunks.is_empty());
    }

    #[test]
    fn chunk_bytes_zero_chunk_size() {
        let data = vec![0x01, 0x02];
        let chunks = chunk_bytes(&data, 0);
        assert!(chunks.is_empty());
    }
}
