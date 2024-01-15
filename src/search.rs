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
            ("test/blocks/t4/article1_lu.txt", Hash::from("13406fa591deec7fda88c97db59ee1bdbebe7d3057bb86b607b4971399a8938127ca3a39ceae6fed7b85d6a1e121ae65745a363da622e4b64ea66ff2acf250af6e6b")),
            ("test/blocks/t4/doc-COPY.pdf", Hash::from("1340a682f8186d97501ae75d9e5349afc7ec1a47c9f1065ef438b9c27c754f179dd3bdb284b57136b8adbf4590c262c852996a2c68c024630e0318b9eb608c80c30c")),
            ("test/blocks/t4/doc.pdf", Hash::from("1340a682f8186d97501ae75d9e5349afc7ec1a47c9f1065ef438b9c27c754f179dd3bdb284b57136b8adbf4590c262c852996a2c68c024630e0318b9eb608c80c30c")),
            ("test/blocks/t4/article1_en.txt", Hash::from("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6")),
            ("test/blocks/t4/article1_de.txt", Hash::from("134036b571f30cddf16459ae40eee97f8c26c5dba21aa1664671eef904f1f02c62c6822ed71878f582e620dc00bc55112ace133a90d51b458209cc7ae61fc279eb69")),
            ("test/blocks/t4/article1_fr.txt", Hash::from("1340ff5c624b6ee1d0ac5f62cd4b810e27520b5ed81df05a62990df8d19d4d7fe341a3d27d51b9fdc571fb02aaffc08f7ee9c9016e8f3e1807a8e12923a8cff87853")),
        ];

        let mut idx = FilenameSearchIndex::new();
        for (filename, hash) in pairs {
            idx.add_filename(filename, &hash);
        }

        let res = idx.search("test/blocks/t4/article1_lu.txt");
        let expected = HashSet::from([
            Hash::from("13406fa591deec7fda88c97db59ee1bdbebe7d3057bb86b607b4971399a8938127ca3a39ceae6fed7b85d6a1e121ae65745a363da622e4b64ea66ff2acf250af6e6b"),
        ]);
        assert_eq!(res, expected);

        let res = idx.search("test/blocks/t4/doc-COPY2.pdf");
        let expected = HashSet::new();
        assert_eq!(res, expected);

        let res = idx.search("test/blocks/t4/article1_fr.txt");
        let expected = HashSet::from([
            Hash::from("1340ff5c624b6ee1d0ac5f62cd4b810e27520b5ed81df05a62990df8d19d4d7fe341a3d27d51b9fdc571fb02aaffc08f7ee9c9016e8f3e1807a8e12923a8cff87853"),
        ]);
        assert_eq!(res, expected);
    }
}
