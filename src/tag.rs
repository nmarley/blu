use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io;

use crate::age::BlackBox;
use crate::compression::{compress, decompress};
use crate::hash::Hash;
use crate::io::{gen_std_bbserde, BlackBoxSerializable};

/// default filename for the tag index file
pub const TAG_INDEX_FILENAME: &str = "tags.dat";

// TODO: advanced tag query syntax and method
// e.g.: Show all BR passports for John, and all Carl's passports
//        (br & passport & john) | (carl & passport)

/// TagIndex is a struct that stores a mapping of tags to files and files to
/// tags.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Eq, Default)]
pub struct TagIndex {
    // fileref hash (hash of entire file) + tags as a hashset of strings
    pub(crate) file_tags: HashMap<Hash, HashSet<String>>,

    /// string -> hashset<file hash>
    pub(crate) tag_files: HashMap<String, HashSet<Hash>>,
}

impl TagIndex {
    /// Create a new TagIndex
    pub fn new() -> Self {
        Self {
            file_tags: HashMap::new(),
            tag_files: HashMap::new(),
        }
    }

    /// Accept a string for a tag and return a sanitized version of it
    fn sanitize_tag(tag: &str) -> String {
        // lowercase and kebab-case the tag, trim all whitespace
        tag.to_lowercase()
            .trim()
            .replace("  ", " ")
            .replace(' ', "-")
    }

    /// Return a list of tags for a given file hash
    /// TODO: should it be a hashset instead?
    pub fn get_tags(&self, hash: &Hash) -> Vec<String> {
        match self.file_tags.get(hash) {
            Some(tags) => tags.iter().map(|tag| tag.to_string()).collect(),
            None => vec![],
        }
    }

    /// Return a list of all tags in the tag index
    /// TODO: should this also be an iterator (similar to `search`)?
    pub fn list_all_tags(&self) -> HashSet<String> {
        self.tag_files.keys().cloned().collect()
    }

    /// Search for a tag and return an iterator over the file hashes
    pub fn search(&self, tag: &str) -> impl Iterator<Item = &Hash> {
        // `into_iter` is used to convert the Option returned by get
        // into an iterator, and `flatten` is used to flatten the
        // iterator of iterators into a single iterator.
        self.tag_files.get(tag).into_iter().flatten()
    }

    /// Iterate over all the file hashes in the tag index
    pub fn iter_filerefs(&self) -> impl Iterator<Item = &Hash> {
        self.file_tags.keys()
    }

    /// Add a tag to a file hash
    pub fn add_tag(&mut self, hash: &Hash, tag: &str) {
        let tag = &Self::sanitize_tag(tag);

        self.file_tags
            .entry((*hash).clone())
            .or_default()
            .insert(tag.to_string());
        self.tag_files
            .entry(tag.to_string())
            .or_default()
            .insert((*hash).clone());
    }

    /// Remove a tag from a file hash
    pub fn remove_tag(&mut self, hash: &Hash, tag: &str) {
        let tag = &Self::sanitize_tag(tag);

        self.file_tags.entry((*hash).clone()).and_modify(|tags| {
            tags.remove(tag);
        });
        let mut should_remove_tag = false;
        self.tag_files.entry(tag.to_string()).and_modify(|hashes| {
            hashes.remove(hash);
            if hashes.is_empty() {
                should_remove_tag = true;
            }
        });
        // remove the tag entirely if there are no more hashes associated with it
        if should_remove_tag {
            self.tag_files.remove(tag);
        }
    }

    /// Remove all tags from a file hash
    pub fn drop_all_tags(&mut self, hash: &Hash) {
        let mut tags: Vec<String> = vec![];
        if let Some((_h, file_tags)) = self.file_tags.remove_entry(hash) {
            tags = file_tags.iter().map(|tag| tag.to_string()).collect();
        }
        for tag in tags.iter() {
            self.remove_tag(hash, tag);
        }
    }
}

gen_std_bbserde!(TagIndex);

#[cfg(test)]
mod test {
    use super::{HashSet, TagIndex};
    use crate::hash::Hash;

    #[test]
    fn tag_index() {
        let mut tag_index = TagIndex::new();

        let my_hash = Hash::from("1340aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        tag_index.add_tag(&my_hash, "test");
        tag_index.add_tag(&my_hash, "passport");
        tag_index.add_tag(&my_hash, "brazil");

        let expected: HashSet<String> =
            HashSet::from(["test".into(), "passport".into(), "brazil".into()]);
        let actual: HashSet<String> =
            HashSet::from_iter(tag_index.get_tags(&my_hash).iter().cloned());
        assert_eq!(expected, actual);

        assert_eq!(expected, tag_index.list_all_tags());
        tag_index.remove_tag(&my_hash, "test");

        let my_hash_b = Hash::from("1340bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        tag_index.add_tag(&my_hash_b, "Peanut Butter ");

        let expected: HashSet<String> = HashSet::from(["passport".into(), "brazil".into()]);
        let actual: HashSet<String> =
            HashSet::from_iter(tag_index.get_tags(&my_hash).iter().cloned());
        assert_eq!(expected, actual);
        let expected_all_tags: HashSet<String> =
            HashSet::from(["passport".into(), "brazil".into(), "peanut-butter".into()]);
        assert_eq!(expected_all_tags, tag_index.list_all_tags());

        tag_index.drop_all_tags(&my_hash);
        let expected: HashSet<String> = HashSet::new();
        let actual: HashSet<String> =
            HashSet::from_iter(tag_index.get_tags(&my_hash).iter().cloned());
        assert_eq!(expected, actual);

        let expected_all_tags: HashSet<String> = HashSet::from(["peanut-butter".into()]);
        assert_eq!(expected_all_tags, tag_index.list_all_tags());
    }

    #[test]
    fn sanitize() {
        // lowercase and kebab-case the tag, trim all whitespace
        assert_eq!(&TagIndex::sanitize_tag("  Test  Tag  "), "test-tag");
        assert_eq!(&TagIndex::sanitize_tag("Peanut butter"), "peanut-butter");
    }

    #[test]
    fn tag_search() {
        let mut tag_index = TagIndex::new();

        // passports
        let ca_passport = Hash::from("1340aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let nz_passport = Hash::from("1340bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let us_passport = Hash::from("1340cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc");

        tag_index.add_tag(&ca_passport, "passport");
        tag_index.add_tag(&ca_passport, "canada");
        tag_index.add_tag(&ca_passport, "mike");

        tag_index.add_tag(&nz_passport, "passport");
        tag_index.add_tag(&nz_passport, "new-zealand");
        tag_index.add_tag(&nz_passport, "leon");

        tag_index.add_tag(&us_passport, "passport");
        tag_index.add_tag(&us_passport, "united-states");
        tag_index.add_tag(&us_passport, "leon");

        let us_birth_cert = Hash::from("1340dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd");
        tag_index.add_tag(&us_birth_cert, "birth-cert");
        tag_index.add_tag(&us_birth_cert, "united-states");
        tag_index.add_tag(&us_birth_cert, "arizona");
        tag_index.add_tag(&us_birth_cert, "citizenship");
        tag_index.add_tag(&us_birth_cert, "usa");

        // test search method
        let expected: HashSet<Hash> = HashSet::from([
            ca_passport.clone(),
            nz_passport.clone(),
            us_passport.clone(),
        ]);
        let actual = tag_index.search("passport").cloned().collect();
        assert_eq!(expected, actual);

        let expected: HashSet<Hash> = HashSet::from([us_birth_cert.clone(), us_passport.clone()]);
        let actual = tag_index.search("united-states").cloned().collect();
        assert_eq!(expected, actual);

        let expected: HashSet<Hash> = HashSet::from([ca_passport.clone()]);
        let actual = tag_index.search("mike").cloned().collect();
        assert_eq!(expected, actual);

        let expected: HashSet<Hash> = HashSet::new();
        let actual = tag_index.search("jim").cloned().collect();
        assert_eq!(expected, actual);
    }
}
