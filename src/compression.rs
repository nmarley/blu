// TODO: prep for std removal from library if possible
// #![cfg_attr(not(test), no_std)]

use flate2::bufread::{GzDecoder, GzEncoder};
use flate2::Compression;
use std::io::{self, Read};

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

#[cfg(test)]
mod test {
    use super::{compress, decompress};
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
}
