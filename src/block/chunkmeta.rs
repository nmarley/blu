use serde::{Deserialize, Serialize};

use crate::hash::{self, Hash};

// ChunkMeta is the hash of a chunk of data and the size of the data, before hashing
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Eq, Ord, PartialOrd)]
pub struct ChunkMeta {
    pub hash: Hash,
    pub size: usize,
}

impl ChunkMeta {
    pub fn new(data: &[u8]) -> Self {
        let mh = hash::multihash(data);
        Self {
            hash: Hash::from(mh.to_bytes()),
            size: data.len(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.hash.to_bytes()
    }
}
