use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

use super::ChunkMeta;

/// FileRef is a container encapsulating a Vec<ChunkMeta> (collection of hashes
/// of chunks read from a fs::File) and filesystem references to it (filenames)
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Eq)]
pub struct FileRef {
    // TODO: Should we add the full file hash here also? e.g.:
    // pub hash: Hash,
    // TODO: ask OpenAI / GPT3 the above ^^^
    pub chunkmetas: Vec<ChunkMeta>,
    pub paths: HashSet<PathBuf>,
    // TODO: filetype, tags, notes?
}

impl PartialOrd for FileRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FileRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.chunkmetas.cmp(&other.chunkmetas)
    }
}

impl FileRef {
    pub fn new(f: &[ChunkMeta]) -> Self {
        Self {
            chunkmetas: f.to_vec(),
            paths: HashSet::new(),
        }
    }

    pub fn get_a_path(&self) -> PathBuf {
        self.paths.iter().next().unwrap().to_path_buf()
    }

    pub fn total_size(&self) -> u64 {
        self.chunkmetas
            .iter()
            .fold(0, |acc, elem| acc + elem.size as u64)
    }
}
