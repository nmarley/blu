use crate::hash;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const DEFAULT_CHUNKFILE_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ChunkFile {
    chunks: Vec<Vec<u8>>,
    capacity: usize,
    positions: HashMap<Vec<u8>, usize>,
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

    pub fn add_chunk(&mut self, chunk: &[u8]) -> Result<(), String> {
        if self.count() >= self.capacity {
            return Err("capacity has been reached".into());
        }

        let index = self.count();
        let hash = hash::hash(chunk);
        self.positions.insert(hash.to_bytes(), index);

        self.chunks.push(chunk.to_vec());
        Ok(())
    }

    pub fn count(&self) -> usize {
        self.chunks.len()
    }

    pub fn get_chunk(&self, index: usize) -> Result<Vec<u8>, String> {
        if index >= self.capacity {
            return Err(format!(
                "index {} greater than capacity of {}",
                index, self.capacity
            ));
        }
        Ok(self.chunks[index].to_vec())
    }

    pub fn get_index_for_hash(&self, hash: &[u8]) -> Option<usize> {
        self.positions.get(hash).map(|e| *e)
    }

    fn serialize(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let encoded: Vec<u8> = bincode::serialize(self)?;
        // let encoded: Vec<u8> = serde_cbor::to_vec(self)?;
        Ok(encoded)
    }

    fn deserialize(data: &[u8]) -> Result<ChunkFile, Box<dyn std::error::Error>> {
        // let decoded: ChunkFile = serde_cbor::from_slice(data)?;
        let decoded: ChunkFile = bincode::deserialize(data)?;
        Ok(decoded)
    }
}

// impl Default for ChunkFile { }

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn capacity() {
        let vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let mut cf = ChunkFile::with_capacity(3);
        // load w/some data
        for v in vec.iter() {
            // dbg!(&v);
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
        let vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let mut cf = ChunkFile::new();
        // load w/some data
        for v in vec.iter() {
            // dbg!(&v);
            cf.add_chunk(v).unwrap();
        }

        let ser = cf.serialize().unwrap();
        // dbg!(&hex::encode(&ser));

        let deser = ChunkFile::deserialize(&ser).unwrap();
        assert_eq!(cf, deser);
    }
}

fn main() {}
