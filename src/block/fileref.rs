use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

use super::ChunkMeta;

/// FileRef is a container encapsulating a Vec<ChunkMeta> (collection of hashes
/// of chunks read from a fs::File) and filesystem references to it (filenames)
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Eq)]
pub struct FileRef {
    pub chunkmetas: Vec<ChunkMeta>,
    pub paths: HashSet<PathBuf>,
    // TODO: filetype, tags, notes?
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
}
