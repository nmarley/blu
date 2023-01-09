use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;

use crate::age::BlackBox;
use crate::compression::{compress, decompress};
use crate::hash::Hash;
use crate::io::{gen_std_bbserde, BlackBoxSerializable};

// TODO: Replace with pure-Rust suffix array (+lcd array) implementation WHICH
// CAN BE SERIALIZED.

#[derive(Default, Debug, PartialEq, Clone, Serialize, Deserialize, Eq)]
/// A node in the trie.
pub struct TrieNode {
    /// The children of the node
    pub children: HashMap<char, TrieNode>,
    /// Whether the node is the end of a word
    pub is_end: bool,
    /// The hash of the file if the node is the end of a word
    pub hash_opt: Option<Hash>,
}

impl TrieNode {
    /// Create a new TrieNode
    pub fn new() -> TrieNode {
        Self {
            children: HashMap::new(),
            is_end: false,
            hash_opt: None,
        }
    }

    /// Add a filename + hash to the trie
    pub fn add(&mut self, filename: &str, hash: &Hash) {
        let mut current = self;
        for c in filename.chars() {
            current = current.children.entry(c).or_default()
        }
        current.is_end = true;
        current.hash_opt = Some(hash.clone());
    }

    /// Search for a filename in the trie
    pub fn search(&self, word: &str) -> Option<Hash> {
        let mut current = self;
        for c in word.chars() {
            match current.children.get(&c) {
                Some(node) => current = node,
                None => return None,
            }
        }
        current.hash_opt.clone()
    }
}

gen_std_bbserde!(TrieNode);

#[cfg(test)]
mod test {
    use super::TrieNode;
    use crate::hash::Hash;

    #[test]
    fn trie_search_exact_filename() {
        let pairs = vec![
            ("test/blocks/t4/article1_lu.txt", Hash::from("13406fa591deec7fda88c97db59ee1bdbebe7d3057bb86b607b4971399a8938127ca3a39ceae6fed7b85d6a1e121ae65745a363da622e4b64ea66ff2acf250af6e6b")),
            ("test/blocks/t4/doc-COPY.pdf", Hash::from("1340a682f8186d97501ae75d9e5349afc7ec1a47c9f1065ef438b9c27c754f179dd3bdb284b57136b8adbf4590c262c852996a2c68c024630e0318b9eb608c80c30c")),
            ("test/blocks/t4/doc.pdf", Hash::from("1340a682f8186d97501ae75d9e5349afc7ec1a47c9f1065ef438b9c27c754f179dd3bdb284b57136b8adbf4590c262c852996a2c68c024630e0318b9eb608c80c30c")),
            ("test/blocks/t4/article1_en.txt", Hash::from("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6")),
            ("test/blocks/t4/article1_de.txt", Hash::from("134036b571f30cddf16459ae40eee97f8c26c5dba21aa1664671eef904f1f02c62c6822ed71878f582e620dc00bc55112ace133a90d51b458209cc7ae61fc279eb69")),
            ("test/blocks/t4/article1_fr.txt", Hash::from("1340ff5c624b6ee1d0ac5f62cd4b810e27520b5ed81df05a62990df8d19d4d7fe341a3d27d51b9fdc571fb02aaffc08f7ee9c9016e8f3e1807a8e12923a8cff87853")),
        ];

        let mut root = TrieNode::new();
        for (filename, hash) in pairs {
            root.add(filename, &hash);
        }

        let res = root.search("test/blocks/t4/article1_lu.txt");
        let expected = Some(Hash::from("13406fa591deec7fda88c97db59ee1bdbebe7d3057bb86b607b4971399a8938127ca3a39ceae6fed7b85d6a1e121ae65745a363da622e4b64ea66ff2acf250af6e6b"));
        assert_eq!(res, expected);

        let res = root.search("test/blocks/t4/doc-COPY2.pdf");
        let expected = None;
        assert_eq!(res, expected);

        let res = root.search("test/blocks/t4/article1_fr.txt");
        let expected = Some(Hash::from("1340ff5c624b6ee1d0ac5f62cd4b810e27520b5ed81df05a62990df8d19d4d7fe341a3d27d51b9fdc571fb02aaffc08f7ee9c9016e8f3e1807a8e12923a8cff87853"));
        assert_eq!(res, expected);
    }
}
