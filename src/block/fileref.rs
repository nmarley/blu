use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

use super::ChunkMeta;
use crate::error::BluError;

/// FileRef is a container encapsulating a Vec<ChunkMeta> (collection of hashes
/// of chunks read from a fs::File) and filesystem references to it (filenames)
#[derive(PartialEq, Clone, Serialize, Deserialize, Eq)]
pub struct FileRef {
    /// Ordered list of chunk hashes and sizes that make up this file
    pub chunkmetas: Vec<ChunkMeta>,
    /// Set of filesystem paths that share this exact content
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
    /// Create a new FileRef with the given chunkmetas and an empty
    /// paths set.
    pub fn new(f: Vec<ChunkMeta>) -> Self {
        Self {
            chunkmetas: f,
            paths: HashSet::new(),
        }
    }

    /// Return any one path from the paths set, or an error if the set
    /// is empty.
    pub fn get_a_path(&self) -> Result<PathBuf, BluError> {
        self.paths
            .iter()
            .next()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| BluError::IndexCorrupted("fileref has no paths".into()))
    }

    /// Compute the total file size in bytes by summing all chunk sizes.
    pub fn total_size(&self) -> u64 {
        self.chunkmetas
            .iter()
            .fold(0, |acc, elem| acc + elem.size as u64)
    }
}
