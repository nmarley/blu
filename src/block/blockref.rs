use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::hash::Hash;

// blockref -> option<enc hash>
//          -> set of references to chunk on disk
/// BlockRef has a collection of file hashes which reference a particular block.
#[derive(Default, Debug, PartialEq, Clone, Serialize, Deserialize, Eq)]
pub struct BlockRef {
    // on-disk locations where this block can be read if necessary
    pub references: HashSet<FileRefLocationIndex>,
}

impl BlockRef {
    pub fn new() -> Self {
        Self {
            references: HashSet::new(),
        }
    }
}

/// FileRefLocationIndex gives the location of a chunk within a FileRef
/// (identified by file hash), with a byte offset and number of bytes to be
/// read.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Ord, PartialOrd, Eq, Hash)]
pub struct FileRefLocationIndex {
    pub file_hash: Hash,
    pub offset: usize,
    pub size: usize,
}
