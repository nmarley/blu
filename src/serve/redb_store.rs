//! redb-backed local index store for `blu serve`.
//!
//! The local redb database is the working copy; encrypted CBOR on the
//! backend is the source of truth and the interchange format. redb
//! pages data in and out through the OS page cache, so the daemon does
//! not pin hundreds of megabytes of deserialized HashMaps in resident
//! memory.
//!
//! Four tables map directly onto the existing index types:
//!
//! | Table | Key | Value | Source |
//! |-------|-----|-------|--------|
//! | `path_index` | relative path (`&str`) | file hash bytes (`&[u8]`) | `PlainIndex::build_path_index()` |
//! | `file_index` | file hash bytes (`&[u8]`) | `FileRef` CBOR (`&[u8]`) | `PlainIndex::files` |
//! | `blob_index` | chunk hash bytes (`&[u8]`) | `BlobBlockLocation` CBOR (`&[u8]`) | `BlobIndex::map` |
//! | `tag_index` | tag string (`&str`) | `HashSet<Hash>` CBOR (`&[u8]`) | `TagIndex::tag_files` |
//!
//! Values are CBOR-encoded via `EncryptedSerializable::serialize_bytes()`
//! / `deserialize_bytes()`. Keys use raw bytes (hash) or UTF-8 strings
//! (paths, tags) for efficient range scans and prefix queries.

use std::collections::HashSet;
use std::ops::Bound;
use std::path::Path;

use redb::{Database, ReadableDatabase, ReadableTableMetadata, TableDefinition};

use crate::blob::BlobBlockLocation;
use crate::blob::BlobIndex;
use crate::block::FileRef;
use crate::block::PlainIndex;
use crate::error::BluError;
use crate::hash::Hash;
use crate::tag::TagIndex;

/// path -> file_hash. Keys are relative user paths (UTF-8).
const PATH_INDEX: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("path_index");

/// file_hash -> FileRef CBOR. Keys are raw multihash bytes.
const FILE_INDEX: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("file_index");

/// chunk_hash -> BlobBlockLocation CBOR. Keys are raw multihash bytes.
const BLOB_INDEX: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("blob_index");

/// tag -> file_hashes CBOR. Keys are sanitized tag strings (UTF-8).
const TAG_INDEX: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("tag_index");

/// redb database handle held by the serve daemon for the lifetime of
/// the process.
pub struct RedbStore {
    db: Database,
}

impl RedbStore {
    /// Open an existing redb database, or create one at the given path
    /// if it does not exist. The parent directory must already exist.
    /// All four tables are created if they do not already exist.
    pub fn open(path: &Path) -> Result<Self, BluError> {
        let db = Database::create(path)?;

        // Create tables eagerly so subsequent read transactions do not
        // hit TableDoesNotExist on a fresh database.
        let txn = db.begin_write()?;
        {
            let _ = txn.open_table(PATH_INDEX)?;
            let _ = txn.open_table(FILE_INDEX)?;
            let _ = txn.open_table(BLOB_INDEX)?;
            let _ = txn.open_table(TAG_INDEX)?;
        }
        txn.commit()?;

        Ok(Self { db })
    }

    /// Bulk-insert all entries from the three deserialized indexes into
    /// redb. This is the "fresh machine" path: pull encrypted indexes
    /// from backend, decrypt+deserialize, load into redb.
    ///
    /// Any existing entries in redb are replaced.
    pub fn populate_from_indexes(
        &self,
        plain: &PlainIndex,
        blob: &BlobIndex,
        tag: &TagIndex,
    ) -> Result<(), BluError> {
        let txn = self.db.begin_write()?;
        {
            let mut path_table = txn.open_table(PATH_INDEX)?;
            let mut file_table = txn.open_table(FILE_INDEX)?;

            for (file_hash, fileref) in plain.files_map_ref() {
                let key = file_hash.to_bytes();
                let value = serialize_cbor(fileref)?;
                file_table.insert(key.as_slice(), value.as_slice())?;

                for path in &fileref.paths {
                    let path_str = path.to_string_lossy();
                    path_table.insert(path_str.as_ref(), key.as_slice())?;
                }
            }
        }
        {
            let mut blob_table = txn.open_table(BLOB_INDEX)?;
            for (chunk_hash, location) in &blob.map {
                let key = chunk_hash.to_bytes();
                let value = serialize_cbor(location)?;
                blob_table.insert(key.as_slice(), value.as_slice())?;
            }
        }
        {
            let mut tag_table = txn.open_table(TAG_INDEX)?;
            for tag_name in tag.list_all_tags() {
                let file_hashes: Vec<Hash> = tag.search(&tag_name).cloned().collect();
                let value = serialize_tag_value(&file_hashes)?;
                tag_table.insert(tag_name.as_str(), value.as_slice())?;
            }
        }
        txn.commit()?;

        Ok(())
    }

    /// Look up a file hash by its virtual path. Returns the raw
    /// multihash bytes of the file hash, or `None` if the path is not
    /// in the index.
    pub fn get_file_hash_by_path(&self, path: &str) -> Result<Option<Hash>, BluError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(PATH_INDEX)?;
        match table.get(path)? {
            Some(guard) => {
                let bytes = guard.value();
                Ok(Some(Hash::from(bytes)))
            }
            None => Ok(None),
        }
    }

    /// Look up a `FileRef` by file hash. Returns `None` if the hash is
    /// not in the index.
    pub fn get_fileref(&self, file_hash: &Hash) -> Result<Option<FileRef>, BluError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILE_INDEX)?;
        let key = file_hash.to_bytes();
        match table.get(key.as_slice())? {
            Some(guard) => {
                let fileref = deserialize_cbor(guard.value())?;
                Ok(Some(fileref))
            }
            None => Ok(None),
        }
    }

    /// Look up a `BlobBlockLocation` by chunk hash. Returns `None` if
    /// the chunk is not in the blob index.
    pub fn get_blob_location(
        &self,
        chunk_hash: &Hash,
    ) -> Result<Option<BlobBlockLocation>, BluError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(BLOB_INDEX)?;
        let key = chunk_hash.to_bytes();
        match table.get(key.as_slice())? {
            Some(guard) => {
                let location = deserialize_cbor(guard.value())?;
                Ok(Some(location))
            }
            None => Ok(None),
        }
    }

    /// Look up the set of file hashes for a given tag. Returns an empty
    /// set if the tag is not in the index.
    pub fn get_file_hashes_for_tag(&self, tag: &str) -> Result<HashSet<Hash>, BluError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(TAG_INDEX)?;
        match table.get(tag)? {
            Some(guard) => {
                let hashes = deserialize_tag_value(guard.value())?;
                Ok(hashes)
            }
            None => Ok(HashSet::new()),
        }
    }

    /// List paths under a prefix, in lexicographic (UTF-8 byte) order.
    ///
    /// Returns up to `limit` `(path, file_hash)` pairs whose keys start
    /// with `prefix`. If `start_after` is provided, listing begins after
    /// that key (exclusive); otherwise it begins at the prefix itself
    /// (inclusive). This is the core primitive for `ListObjectsV2`.
    ///
    /// redb's btree returns keys in lexicographic byte order, which
    /// matches S3's required sort order for object listings.
    pub fn list_paths(
        &self,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Hash)>, BluError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(PATH_INDEX)?;

        let start_bound = match start_after {
            Some(key) => Bound::Excluded(key),
            None => Bound::Included(prefix),
        };
        let next = next_prefix(prefix);
        let end_bound = match &next {
            Some(s) => Bound::Excluded(s.as_str()),
            None => Bound::Unbounded,
        };

        let mut results = Vec::with_capacity(limit);
        if limit == 0 {
            return Ok(results);
        }
        for item in table.range::<&str>((start_bound, end_bound))? {
            let (key_guard, value_guard) = item?;
            let path = key_guard.value().to_string();
            let hash = Hash::from(value_guard.value());
            results.push((path, hash));
            if results.len() >= limit {
                break;
            }
        }
        Ok(results)
    }

    /// Count the number of entries in the path index.
    pub fn path_count(&self) -> Result<u64, BluError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(PATH_INDEX)?;
        Ok(table.len()?)
    }

    /// Count the number of entries in the file index.
    pub fn file_count(&self) -> Result<u64, BluError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILE_INDEX)?;
        Ok(table.len()?)
    }

    /// Count the number of entries in the blob index.
    pub fn blob_count(&self) -> Result<u64, BluError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(BLOB_INDEX)?;
        Ok(table.len()?)
    }

    /// Count the number of entries in the tag index.
    pub fn tag_count(&self) -> Result<u64, BluError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(TAG_INDEX)?;
        Ok(table.len()?)
    }
}

/// Serialize a `Vec<Hash>` as CBOR for the tag index value.
fn serialize_tag_value(hashes: &[Hash]) -> Result<Vec<u8>, BluError> {
    serialize_cbor(hashes)
}

/// Deserialize a `Vec<Hash>` from CBOR for the tag index value.
fn deserialize_tag_value(data: &[u8]) -> Result<HashSet<Hash>, BluError> {
    let hashes: Vec<Hash> = deserialize_cbor(data)?;
    Ok(hashes.into_iter().collect())
}

/// Serialize any `Serialize` type to CBOR bytes.
fn serialize_cbor<T: serde::Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, BluError> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf)
        .map_err(|e| BluError::SerializationError(e.to_string()))?;
    Ok(buf)
}

/// Deserialize any `Deserialize` type from CBOR bytes.
fn deserialize_cbor<T: for<'de> serde::Deserialize<'de>>(data: &[u8]) -> Result<T, BluError> {
    ciborium::from_reader(data).map_err(|e| BluError::DeserializationError(e.to_string()))
}

/// Compute the next lexicographic prefix for a UTF-8 string.
///
/// Returns `Some(next)` where `next` is the smallest string that is
/// lexicographically greater than all strings starting with `prefix`.
/// This is used as the exclusive upper bound for a prefix scan: all keys
/// in the range `[prefix, next)` share the given prefix.
///
/// Returns `None` when there is no lexicographic successor (every byte
/// either is `0xFF` or would produce invalid UTF-8 when incremented),
/// meaning the caller should use an unbounded upper bound instead.
///
/// The algorithm increments the last byte of the UTF-8 representation.
/// If the result is not valid UTF-8 (e.g., incrementing a single-byte
/// char's `0x7F` to `0x80`, or a continuation byte past `0xBF`), that
/// byte is removed and the process recurses on the remaining prefix.
/// This is the standard lexicographic-prefix-termination trick, adapted
/// for UTF-8 validity.
fn next_prefix(prefix: &str) -> Option<String> {
    let mut bytes = prefix.as_bytes().to_vec();
    while let Some(last) = bytes.last_mut() {
        if *last == 0xFF {
            bytes.pop();
            continue;
        }
        *last += 1;
        if std::str::from_utf8(&bytes).is_ok() {
            return Some(
                String::from_utf8(bytes).expect("checked valid UTF-8 before constructing String"),
            );
        }
        bytes.pop();
    }
    None
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use super::*;
    use crate::block::ChunkMeta;
    use crate::io::Position;

    // Reuse the well-known test hashes from block/index tests.
    const HASH_A: &str =
        "1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726";
    const HASH_B: &str =
        "134089e75f89ca624a073a1b3648303a4abd77fd49325110aa08d683ea0a03de6f949650bbf74f33597f5dcc54c57aaeb47cd143452a320f06c69829c54dc7d9dbb5";
    const HASH_FILE1: &str =
        "13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0";

    fn test_plain_index() -> PlainIndex {
        let chunk_a = ChunkMeta {
            hash: Hash::from(HASH_A),
            size: 4096,
        };
        let chunk_b = ChunkMeta {
            hash: Hash::from(HASH_B),
            size: 4096,
        };

        let file1_ref = FileRef {
            chunkmetas: vec![chunk_a.clone(), chunk_b.clone()],
            paths: HashSet::from(["docs/readme.txt".into()]),
        };
        let file2_ref = FileRef {
            chunkmetas: vec![chunk_a],
            paths: HashSet::from(["photos/img.jpg".into(), "photos/copy.jpg".into()]),
        };

        let mut plain = PlainIndex::new_empty();
        plain.files.insert(Hash::from(HASH_FILE1), file1_ref);
        plain.files.insert(Hash::from(HASH_A), file2_ref);
        plain
    }

    fn test_blob_index() -> BlobIndex {
        let mut blob = BlobIndex::default();

        let location_a = BlobBlockLocation::new(
            PathBuf::from("d/dd4/dd4ce/dd4ce38e"),
            Position {
                offset: 0,
                size: 4096,
            },
        );
        let location_b = BlobBlockLocation::new(
            PathBuf::from("d/dd4/dd4ce/dd4ce38e"),
            Position {
                offset: 4096,
                size: 4096,
            },
        );

        blob.add_chunk_location(&Hash::from(HASH_A), &location_a);
        blob.add_chunk_location(&Hash::from(HASH_B), &location_b);
        blob
    }

    fn test_tag_index() -> TagIndex {
        let mut tag = TagIndex::new();
        tag.add_tag(&Hash::from(HASH_FILE1), "important");
        tag.add_tag(&Hash::from(HASH_FILE1), "docs");
        tag.add_tag(&Hash::from(HASH_A), "photos");
        tag
    }

    #[test]
    fn round_trip_all_tables() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&tmp.path().join("test.redb")).unwrap();

        let plain = test_plain_index();
        let blob = test_blob_index();
        let tag = test_tag_index();

        store.populate_from_indexes(&plain, &blob, &tag).unwrap();

        // Verify counts.
        assert_eq!(store.path_count().unwrap(), 3);
        assert_eq!(store.file_count().unwrap(), 2);
        assert_eq!(store.blob_count().unwrap(), 2);
        assert_eq!(store.tag_count().unwrap(), 3);

        // Verify path -> file_hash lookups.
        let hash_from_path = store
            .get_file_hash_by_path("docs/readme.txt")
            .unwrap()
            .unwrap();
        assert_eq!(hash_from_path, Hash::from(HASH_FILE1));

        let hash_from_path2 = store
            .get_file_hash_by_path("photos/img.jpg")
            .unwrap()
            .unwrap();
        assert_eq!(hash_from_path2, Hash::from(HASH_A));

        assert!(store
            .get_file_hash_by_path("nonexistent")
            .unwrap()
            .is_none());

        // Verify file_hash -> FileRef lookups.
        let fileref = store.get_fileref(&Hash::from(HASH_FILE1)).unwrap().unwrap();
        assert_eq!(fileref.chunkmetas.len(), 2);
        assert_eq!(fileref.chunkmetas[0].hash, Hash::from(HASH_A));
        assert_eq!(fileref.chunkmetas[0].size, 4096);
        assert_eq!(fileref.chunkmetas[1].hash, Hash::from(HASH_B));
        assert_eq!(fileref.chunkmetas[1].size, 4096);
        assert!(fileref.paths.contains(&PathBuf::from("docs/readme.txt")));
        assert_eq!(fileref.total_size(), 8192);

        let fileref2 = store.get_fileref(&Hash::from(HASH_A)).unwrap().unwrap();
        assert_eq!(fileref2.chunkmetas.len(), 1);
        assert_eq!(fileref2.total_size(), 4096);
        assert_eq!(fileref2.paths.len(), 2);

        assert!(store
            .get_fileref(&Hash::from("1340deadbeef"))
            .unwrap()
            .is_none());

        // Verify chunk_hash -> BlobBlockLocation lookups.
        let loc = store
            .get_blob_location(&Hash::from(HASH_A))
            .unwrap()
            .unwrap();
        assert_eq!(loc.blob_path(), &PathBuf::from("d/dd4/dd4ce/dd4ce38e"));
        assert_eq!(loc.position.offset, 0);
        assert_eq!(loc.position.size, 4096);

        let loc2 = store
            .get_blob_location(&Hash::from(HASH_B))
            .unwrap()
            .unwrap();
        assert_eq!(loc2.position.offset, 4096);
        assert_eq!(loc2.position.size, 4096);

        assert!(store
            .get_blob_location(&Hash::from("1340deadbeef"))
            .unwrap()
            .is_none());

        // Verify tag -> file_hashes lookups.
        let docs_hashes = store.get_file_hashes_for_tag("docs").unwrap();
        assert_eq!(docs_hashes.len(), 1);
        assert!(docs_hashes.contains(&Hash::from(HASH_FILE1)));

        let photos_hashes = store.get_file_hashes_for_tag("photos").unwrap();
        assert_eq!(photos_hashes.len(), 1);
        assert!(photos_hashes.contains(&Hash::from(HASH_A)));

        let important_hashes = store.get_file_hashes_for_tag("important").unwrap();
        assert_eq!(important_hashes.len(), 1);
        assert!(important_hashes.contains(&Hash::from(HASH_FILE1)));

        // Unknown tag returns empty set.
        let unknown = store.get_file_hashes_for_tag("nonexistent").unwrap();
        assert!(unknown.is_empty());
    }

    #[test]
    fn open_existing_database() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("persist.redb");

        {
            let store = RedbStore::open(&db_path).unwrap();
            let plain = test_plain_index();
            let blob = test_blob_index();
            let tag = test_tag_index();
            store.populate_from_indexes(&plain, &blob, &tag).unwrap();
        }

        // Reopen and verify data persisted.
        let store = RedbStore::open(&db_path).unwrap();
        assert_eq!(store.path_count().unwrap(), 3);
        assert_eq!(store.file_count().unwrap(), 2);
        assert_eq!(store.blob_count().unwrap(), 2);
        assert_eq!(store.tag_count().unwrap(), 3);

        let hash = store
            .get_file_hash_by_path("docs/readme.txt")
            .unwrap()
            .unwrap();
        assert_eq!(hash, Hash::from(HASH_FILE1));
    }

    #[test]
    fn empty_indexes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&tmp.path().join("empty.redb")).unwrap();

        let plain = PlainIndex::new_empty();
        let blob = BlobIndex::default();
        let tag = TagIndex::new();

        store.populate_from_indexes(&plain, &blob, &tag).unwrap();

        assert_eq!(store.path_count().unwrap(), 0);
        assert_eq!(store.file_count().unwrap(), 0);
        assert_eq!(store.blob_count().unwrap(), 0);
        assert_eq!(store.tag_count().unwrap(), 0);

        assert!(store.get_file_hash_by_path("anything").unwrap().is_none());
    }

    fn prefix_store() -> RedbStore {
        let tmp = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&tmp.path().join("prefix.redb")).unwrap();

        let mut plain = PlainIndex::new_empty();
        let paths = [
            "docs/readme.txt",
            "docs/api/intro.md",
            "docs/api/v2.md",
            "docs/changelog.txt",
            "photos/img.jpg",
            "photos/2024/jan/heavy.raw",
            "photos/2024/feb/sunset.raw",
            "photos/copy.jpg",
            "videos/clip.mp4",
            "videos/trailer.mp4",
        ];

        let dummy_chunk = ChunkMeta {
            hash: Hash::from("1340aaaa"),
            size: 100,
        };

        for (i, path) in paths.iter().enumerate() {
            let fileref = FileRef {
                chunkmetas: vec![dummy_chunk.clone()],
                paths: HashSet::from([PathBuf::from(path)]),
            };
            let file_hash = Hash::from(format!("1340{:030x}", i).as_str());
            plain.files.insert(file_hash, fileref);
        }

        let blob = BlobIndex::default();
        let tag = TagIndex::new();
        store.populate_from_indexes(&plain, &blob, &tag).unwrap();
        store
    }

    #[test]
    fn list_paths_full_scan() {
        let store = prefix_store();
        let results = store.list_paths("", None, 100).unwrap();
        assert_eq!(results.len(), 10);
        assert_eq!(results[0].0, "docs/api/intro.md");
        assert_eq!(results[9].0, "videos/trailer.mp4");
    }

    #[test]
    fn list_paths_prefix_filter() {
        let store = prefix_store();
        let results = store.list_paths("docs/", None, 100).unwrap();
        assert_eq!(results.len(), 4);
        assert_eq!(results[0].0, "docs/api/intro.md");
        assert_eq!(results[1].0, "docs/api/v2.md");
        assert_eq!(results[2].0, "docs/changelog.txt");
        assert_eq!(results[3].0, "docs/readme.txt");
    }

    #[test]
    fn list_paths_nested_prefix() {
        let store = prefix_store();
        let results = store.list_paths("docs/api/", None, 100).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "docs/api/intro.md");
        assert_eq!(results[1].0, "docs/api/v2.md");
    }

    #[test]
    fn list_paths_start_after() {
        let store = prefix_store();
        let results = store
            .list_paths("docs/", Some("docs/api/v2.md"), 100)
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "docs/changelog.txt");
        assert_eq!(results[1].0, "docs/readme.txt");
    }

    #[test]
    fn list_paths_limit_truncation() {
        let store = prefix_store();
        let results = store.list_paths("docs/", None, 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "docs/api/intro.md");
        assert_eq!(results[1].0, "docs/api/v2.md");
    }

    #[test]
    fn list_paths_prefix_no_match() {
        let store = prefix_store();
        let results = store.list_paths("nonexistent/", None, 100).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn list_paths_empty_store() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&tmp.path().join("empty.redb")).unwrap();
        let plain = PlainIndex::new_empty();
        let blob = BlobIndex::default();
        let tag = TagIndex::new();
        store.populate_from_indexes(&plain, &blob, &tag).unwrap();

        let results = store.list_paths("", None, 100).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn list_paths_limit_zero() {
        let store = prefix_store();
        let results = store.list_paths("", None, 0).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn next_prefix_simple() {
        assert_eq!(next_prefix("abc"), Some("abd".to_string()));
        assert_eq!(next_prefix("a"), Some("b".to_string()));
        assert_eq!(next_prefix("docs/"), Some("docs0".to_string()));
    }

    #[test]
    fn next_prefix_carry() {
        // "ab~" has last byte 0x7E (~), incrementing gives 0x7F (DEL)
        assert_eq!(next_prefix("ab~"), Some("ab\u{7F}".to_string()));
        // Incrementing 0x7F (DEL) to 0x80 would produce invalid UTF-8
        // (0x80 is a continuation byte), so the byte is popped and the
        // algorithm recurses on "ab", incrementing 'b' to 'c'.
        assert_eq!(next_prefix("ab\u{7F}"), Some("ac".to_string()));
    }

    #[test]
    fn next_prefix_all_ff() {
        // 0xFF is not valid in UTF-8, so use a string whose bytes all
        // either are 0xFF or increment to invalid UTF-8. The string "\xFF"
        // is itself invalid UTF-8 so we cannot construct it as &str.
        // Instead, test with a valid UTF-8 string that has no successor:
        // a string ending in U+10FFFF (the max Unicode scalar value),
        // encoded as 0xF4 0x8F 0xBF 0xBF. Incrementing any byte produces
        // invalid UTF-8, so the whole string is consumed and None is
        // returned.
        let max_char = "\u{10FFFF}";
        assert_eq!(next_prefix(max_char), None);
    }

    #[test]
    fn next_prefix_empty() {
        assert_eq!(next_prefix(""), None);
    }

    #[test]
    fn next_prefix_multibyte_utf8() {
        // The byte-increment operates on the raw UTF-8 bytes. For a
        // multi-byte char like "é" (0xC3 0xA9), incrementing the last
        // byte gives 0xC3 0xAA which is "ê". The result is still valid
        // UTF-8 because we only incremented a non-continuation byte
        // position... actually 0xA9 is a continuation byte. Incrementing
        // it to 0xAA is still a valid continuation byte range (0x80-0xBF),
        // so the result is valid UTF-8: "ê".
        assert_eq!(next_prefix("é"), Some("ê".to_string()));
    }
}
