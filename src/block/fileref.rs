use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

use super::ChunkMeta;

/// FileRef is a container encapsulating a Vec<ChunkMeta> (collection of hashes
/// of chunks read from a fs::File) and filesystem references to it (filenames)
#[derive(PartialEq, Clone, Serialize, Deserialize, Eq)]
pub struct FileRef {
    pub chunkmetas: Vec<ChunkMeta>,
    pub paths: HashSet<PathBuf>,
}

impl std::fmt::Debug for FileRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "FileRef {{ chunkmetas.len(): {}, paths: {:?} }}",
            self.chunkmetas.len(),
            self.paths
        )
    }
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
    pub fn new(f: Vec<ChunkMeta>) -> Self {
        Self {
            chunkmetas: f,
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
