use std::collections::{HashMap, HashSet};
use std::hash::Hasher;
use suffix::SuffixTable;

use crate::hash::Hash;

// TODO: implement serde, to/from file like the other indexes
// default filename for the search index file
// pub const SEARCH_INDEX_FILENAME: &str = "search.dat";

/// MySuffixTable is a wrapper around the SuffixTable struct from the suffix
/// crate, and implements Hash so that it can be used as a hashmap key.
///
/// Might refactor this later.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MySuffixTable {
    table: SuffixTable<'static, 'static>,
}

impl std::hash::Hash for MySuffixTable {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.table.text().hash(state);
        self.table.table().hash(state);
    }
}

impl MySuffixTable {
    fn new(text: &str) -> Self {
        let owned_text = text.to_owned();
        let static_str = Box::leak(owned_text.into_boxed_str()) as &'static str;
        Self {
            table: SuffixTable::new(static_str),
        }
    }

    fn contains(&self, query: &str) -> bool {
        self.table.contains(query)
    }
}

/// FilenameSearchIndex is a struct that stores a mapping of suffix arrays to filenames.
#[derive(Clone, Debug, PartialEq)]
pub struct FilenameSearchIndex {
    suffix_filenames: HashMap<MySuffixTable, Hash>,
}

impl FilenameSearchIndex {
    /// Create a new FilenameSearchIndex
    pub fn new() -> Self {
        Self {
            suffix_filenames: HashMap::new(),
        }
    }

    // TODO: This could be used for tags too (and in this system, filenames are
    // really just another type of tag w/filesystem naming semantics enforced)
    //
    /// Add a filename to the search index
    pub fn add_filename(&mut self, filename: &str, hash: &Hash) {
        self.suffix_filenames
            .insert(MySuffixTable::new(filename), hash.clone());
    }

    /// Search for a filename in the search index
    pub fn search(&self, query: &str) -> HashSet<Hash> {
        self.suffix_filenames
            .iter()
            .filter(|(k, _v)| k.contains(query))
            .map(|(_k, v)| v.clone())
            .collect()
    }
}

impl Default for FilenameSearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;

    use super::FilenameSearchIndex;
    use crate::hash::Hash;

    #[test]
    fn test_search() {
        let pairs = vec![
            (
                "test/blocks/t4/article1_lu.txt",
                Hash::from("1e200c9b0a15a9e13f5de7d62f0c338aaf2bc22235fd32bf1ba8be2db026ccc46b24"),
            ),
            (
                "test/blocks/t4/doc-COPY.pdf",
                Hash::from("1e203cdcf9931fc90beb9fc2136f054179cc2346fcc2d6e67ea405027670c00618f7"),
            ),
            (
                "test/blocks/t4/doc.pdf",
                Hash::from("1e203cdcf9931fc90beb9fc2136f054179cc2346fcc2d6e67ea405027670c00618f7"),
            ),
            (
                "test/blocks/t4/article1_en.txt",
                Hash::from("1e2063d7f0a0f38a10f4b85c36bac72e2880b6a1c2511330cd67bd3e29005553e011"),
            ),
            (
                "test/blocks/t4/article1_de.txt",
                Hash::from("1e203e815acb974cf227739ab663fe335950cf82c13bf7315e97cc4b5d54e00ff17b"),
            ),
            (
                "test/blocks/t4/article1_fr.txt",
                Hash::from("1e208f446be756e70410240a0c8fd38cc6e826f281668e38376924ed387cadaee5ae"),
            ),
        ];

        let mut idx = FilenameSearchIndex::new();
        for (filename, hash) in pairs {
            idx.add_filename(filename, &hash);
        }

        let res = idx.search("test/blocks/t4/article1_lu.txt");
        let expected = HashSet::from([Hash::from(
            "1e200c9b0a15a9e13f5de7d62f0c338aaf2bc22235fd32bf1ba8be2db026ccc46b24",
        )]);
        assert_eq!(res, expected);

        let res = idx.search("test/blocks/t4/doc-COPY2.pdf");
        let expected = HashSet::new();
        assert_eq!(res, expected);

        let res = idx.search("test/blocks/t4/article1_fr.txt");
        let expected = HashSet::from([Hash::from(
            "1e208f446be756e70410240a0c8fd38cc6e826f281668e38376924ed387cadaee5ae",
        )]);
        assert_eq!(res, expected);
    }
}
