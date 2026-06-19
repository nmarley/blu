use serde::{Deserialize, Serialize};

use crate::hash::{self, Hash};

/// ChunkMeta is the hash of a chunk of data and the size of the data,
/// before hashing.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Eq, Ord, PartialOrd)]
pub struct ChunkMeta {
    /// Multihash of the chunk's plaintext content
    pub hash: Hash,
    /// Size of the chunk in bytes (before hashing)
    pub size: usize,
}

impl ChunkMeta {
    /// Create a new ChunkMeta by hashing the given data.
    pub fn new(data: &[u8]) -> Self {
        let mh = hash::multihash(data);
        Self {
            hash: Hash::from(mh.to_bytes()),
            size: data.len(),
        }
    }

    /// Return the raw multihash bytes of this chunk's hash.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.hash.to_bytes()
    }
}
