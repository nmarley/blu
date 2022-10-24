use std::io::{BufRead, BufReader};
use std::path::Path;

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
    buf_reader: BufReader<std::fs::File>,
}

impl Chunkerator {
    pub fn new<P: AsRef<Path>>(
        filepath: P,
        chunk_size: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let f = std::fs::File::open(filepath.as_ref()).unwrap();
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

#[cfg(test)]
mod test {
    use super::Chunkerator;
    // use crate::{BlockRef, ChunkMeta, FileRef, FileRefLocationIndex, Hash, PlainIndex};
    use crate::block::BLOCK_SIZE;
    use std::path::Path;

    const TEST_BLOCKS_DIR_T1: &str = "test/blocks/t1/";

    #[test]
    fn chunkerator() {
        let file5_path = Path::new(TEST_BLOCKS_DIR_T1).join("file5.txt");
        let mut chunker = Chunkerator::new(file5_path, BLOCK_SIZE).unwrap();
        let chunk = chunker.next();
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().len(), 1024);
    }
}
