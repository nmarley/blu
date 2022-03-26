use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::hash::{self, Hash};

const DEFAULT_CHUNKFILE_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ChunkFile {
    // this is a vector of encryted data chunks -- NOT HASHES
    chunks: Vec<Vec<u8>>,
    capacity: usize,

    // this is the hash / index into the chunkfile, e.g. the hash of the
    // encrypted data chunk can be found in `chunks` at index usize>
    positions: HashMap<Hash, usize>,
}

impl ChunkFile {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CHUNKFILE_CAPACITY)
    }
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            chunks: Vec::with_capacity(capacity),
            positions: HashMap::new(),
        }
    }

    pub fn add_chunk(&mut self, chunk: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        if self.count() >= self.capacity {
            return Err("capacity has been reached".into());
        }

        let index = self.count();
        let hash = Hash::from(hash::multihash(chunk).to_bytes());
        self.positions.insert(hash, index);

        self.chunks.push(chunk.to_vec());
        Ok(())
    }

    pub fn count(&self) -> usize {
        self.chunks.len()
    }

    pub fn get_chunk(&self, index: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        if index >= self.capacity {
            return Err(
                format!("index {} greater than capacity of {}", index, self.capacity).into(),
            );
        }
        Ok(self.chunks[index].to_vec())
    }

    pub fn get_index_for_hash(&self, hash: &Hash) -> Option<usize> {
        self.positions.get(hash).copied()
    }

    pub fn serialize(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let encoded: Vec<u8> = bincode::serialize(self)?;
        // let encoded: Vec<u8> = serde_cbor::to_vec(self)?;
        Ok(encoded)
    }

    pub fn deserialize(data: &[u8]) -> Result<ChunkFile, Box<dyn std::error::Error>> {
        // let decoded: ChunkFile = serde_cbor::from_slice(data)?;
        let decoded: ChunkFile = bincode::deserialize(data)?;
        Ok(decoded)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    // helper func used in tests below
    fn test_chunkfile() -> ChunkFile {
        let vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let mut cf = ChunkFile::new();
        // load w/some data
        for v in vec.iter() {
            cf.add_chunk(v).unwrap();
        }
        cf
    }

    #[test]
    fn capacity() {
        // NOTE: do not use `test_chunkfile()` here, as we are testing capacity
        let vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let mut cf = ChunkFile::with_capacity(3);
        // load w/some data
        for v in vec.iter() {
            cf.add_chunk(v).unwrap();
        }
        // test get_chunk also
        assert_eq!(cf.get_chunk(0).unwrap(), vec![0x0b, 0x0a, 0x00]);
        assert_eq!(cf.get_chunk(1).unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);

        // can't get beyond index
        assert!(cf.get_chunk(3).is_err());
        // can't add any more, at capacity
        assert!(cf.add_chunk(&vec[0]).is_err());
    }

    #[test]
    fn serde() {
        let cf = test_chunkfile();
        let ser = cf.serialize().unwrap();
        let deser = ChunkFile::deserialize(&ser).unwrap();
        assert_eq!(cf, deser);
    }

    #[test]
    fn index() {
        let cf = test_chunkfile();
        let hashes_expected = vec![
            ("1340e94518b58bcd5e29a8f6251fbc457c580691c8f9d3e3a17dc404d2e5dc86fa98ac857b8ba9366d6023da1196f89729e760e13fee78c10993c181ecee4211be76", Some(0)),
            ("13401284b2d521535196f22175d5f558104220a6ad7680e78b49fa6f20e57ea7b185d71ec1edb137e70eba528dedb141f5d2f8bb53149d262932b27cf41fed96aa7f", Some(1)),
            ("13401332e5814224318ddcb3db935b3a7af1f97073b50033be1bc729302028e906f4cb12a652eefe76d7d4f2e8d6bf1671b331f76dc93546e9faa395892fe28d241c", Some(2)),
            ("1340cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e", None),
        ];
        for tuple in hashes_expected.into_iter() {
            let (hash, opt) = (Hash::from(tuple.0), tuple.1);
            assert_eq!(cf.get_index_for_hash(&hash), opt);
        }
    }
}
