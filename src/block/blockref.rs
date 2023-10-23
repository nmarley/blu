use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::hash::Hash;
use crate::io::Position;

// blockref -> option<enc hash>
//          -> set of references to chunk on disk
/// BlockRef has a collection of file hashes which reference a particular block.
#[derive(Default, Debug, PartialEq, Clone, Serialize, Deserialize, Eq)]
pub struct BlockRef {
    // on-disk locations where this block can be read if necessary
    pub references: HashMap<Hash, Position>,
}

impl BlockRef {
    pub fn new() -> Self {
        Self {
            references: HashMap::new(),
        }
    }

    // old: return Option<Position> (rv from self.references.remove(file_hash))
    pub fn delete_fileref(&mut self, file_hash: &Hash) -> bool {
        let _opt_pos = self.references.remove(file_hash);
        self.references.is_empty()
    }
}
