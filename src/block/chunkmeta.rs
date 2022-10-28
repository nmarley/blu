use serde::{Deserialize, Serialize};
use std::path::Path;

use super::chunkerator::Chunkerator;
use crate::block::BLOCK_SIZE;
use crate::hash::{self, Hash};

// ChunkMeta is the hash of a chunk of data and the size of the data, before hashing
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Eq)]
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

    // TODO: consider removing this if not used
    pub fn read_from_disk<P: AsRef<Path>>(
        filepath: P,
    ) -> Result<Vec<Self>, Box<dyn std::error::Error>> {
        let chunker = Chunkerator::new(filepath, BLOCK_SIZE)?;
        let chunkmetas: Vec<Self> = chunker.into_iter().map(|e| Self::new(&e)).collect();
        Ok(chunkmetas)
    }
}
