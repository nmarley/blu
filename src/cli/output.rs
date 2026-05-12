use std::path::PathBuf;

use crate::format::human_bytes;
use crate::hash::Hash;

/// FileDisplay is a struct for displaying data in a file in a human-readable
/// format, and contains the file hash idenitifer used in blu, as well as the
/// size and all filesystem paths that point to the file.
#[derive(Clone, Debug)]
pub struct FileDisplay {
    /// The hash of the file, used to uniquely identify it
    pub hash: Hash,
    /// The size of the file in bytes
    pub size: u64,
    /// All filesystem paths that point to the file
    pub paths: Vec<PathBuf>,
}

impl std::fmt::Display for FileDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let display_hash = self.hash.dbg_short(7);
        write!(
            f,
            "hash: {}, size: {}, paths: {:?}",
            display_hash,
            human_bytes(self.size),
            self.paths,
        )
    }
}
