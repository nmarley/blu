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

use std::collections::{HashMap, HashSet};
use std::ops::Bound;
use std::path::{Path, PathBuf};

use chrono::Timelike;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

use crate::blob::BlobBlockLocation;
use crate::blob::BlobIndex;
use crate::block::BlockRef;
use crate::block::FileRef;
use crate::block::PlainIndex;
use crate::error::BluError;
use crate::hash::Hash;
use crate::io::Position;
use crate::tag::TagIndex;

/// path -> file_hash. Keys are relative user paths (UTF-8).
const PATH_INDEX: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("path_index");

/// file_hash -> FileRef CBOR. Keys are raw multihash bytes.
const FILE_INDEX: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("file_index");

/// chunk_hash -> BlobBlockLocation CBOR. Keys are raw multihash bytes.
const BLOB_INDEX: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("blob_index");

/// tag -> file_hashes CBOR. Keys are sanitized tag strings (UTF-8).
const TAG_INDEX: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("tag_index");

/// chunk_hash -> BlockRef CBOR. Keys are raw multihash bytes.
///
/// Maps onto `PlainIndex::blocks` (chunk_hash -> file_hash ->
/// Position). Used by the delete cascade to find which files
/// reference a given chunk without scanning all FileRefs.
const BLOCK_INDEX: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("block_index");

/// redb database handle held by the serve daemon for the lifetime of
/// the process.
pub struct RedbStore {
    db: Database,
}

/// Statistics returned by [`RedbStore::delete_object_index`]. Useful
/// for logging and for the eventual `DeleteObject` response body.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeleteStats {
    /// Number of BlockRef entries deleted because the file was their
    /// last remaining reference.
    pub blocks_removed: usize,
    /// Number of blob_index entries (chunk -> BlobBlockLocation)
    /// removed because their BlockRef became unreferenced.
    pub chunks_removed: usize,
    /// Number of tag_index entries updated to drop this file_hash,
    /// including entries that became empty and were deleted.
    pub tags_touched: usize,
    /// Blob file paths that became fully dead (no surviving chunks)
    /// as a result of this delete cascade. The caller should delete
    /// these from the storage backend after the transaction commits.
    pub blobs_dead: Vec<std::path::PathBuf>,
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
            let _ = txn.open_table(BLOCK_INDEX)?;
        }
        txn.commit()?;

        Ok(Self { db })
    }

    /// Bulk-insert all entries from the deserialized indexes into
    /// redb. This is the "fresh machine" path: pull encrypted indexes
    /// from backend, decrypt+deserialize, load into redb.
    ///
    /// Populates all five tables: path_index, file_index, blob_index,
    /// tag_index, and block_index. Any existing entries in redb are
    /// replaced.
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
            let mut block_table = txn.open_table(BLOCK_INDEX)?;
            for (chunk_hash, blockref) in plain.blocks_map_ref() {
                let key = chunk_hash.to_bytes();
                let value = serialize_cbor(blockref)?;
                block_table.insert(key.as_slice(), value.as_slice())?;
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

    /// Look up a `BlockRef` by chunk hash. Returns `None` if the
    /// chunk is not in the block index.
    ///
    /// The block index maps chunk_hash -> BlockRef, where BlockRef
    /// contains a set of file_hash -> Position references. This is
    /// the reverse mapping of `FileRef::chunkmetas`, used by the
    /// delete cascade to find which files reference a given chunk.
    pub fn get_blockref(&self, chunk_hash: &Hash) -> Result<Option<BlockRef>, BluError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(BLOCK_INDEX)?;
        let key = chunk_hash.to_bytes();
        match table.get(key.as_slice())? {
            Some(guard) => {
                let blockref = deserialize_cbor(guard.value())?;
                Ok(Some(blockref))
            }
            None => Ok(None),
        }
    }

    /// Insert or replace a `BlockRef` for the given chunk hash.
    pub fn put_blockref(&self, chunk_hash: &Hash, blockref: &BlockRef) -> Result<(), BluError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(BLOCK_INDEX)?;
            let key = chunk_hash.to_bytes();
            let value = serialize_cbor(blockref)?;
            table.insert(key.as_slice(), value.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Delete the `BlockRef` for the given chunk hash. Returns `true`
    /// if an entry was removed, `false` if it did not exist.
    pub fn delete_blockref(&self, chunk_hash: &Hash) -> Result<bool, BluError> {
        let txn = self.db.begin_write()?;
        let removed = {
            let mut table = txn.open_table(BLOCK_INDEX)?;
            let key = chunk_hash.to_bytes();
            let guard = table.remove(key.as_slice())?;
            guard.is_some()
        };
        txn.commit()?;
        Ok(removed)
    }

    /// Insert or replace a `BlobBlockLocation` for the given chunk
    /// hash in the blob index.
    pub fn put_blob_location(
        &self,
        chunk_hash: &Hash,
        location: &BlobBlockLocation,
    ) -> Result<(), BluError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(BLOB_INDEX)?;
            let key = chunk_hash.to_bytes();
            let value = serialize_cbor(location)?;
            table.insert(key.as_slice(), value.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Delete the `BlobBlockLocation` for the given chunk hash.
    /// Returns `true` if an entry was removed, `false` if it did not
    /// exist.
    pub fn delete_blob_location(&self, chunk_hash: &Hash) -> Result<bool, BluError> {
        let txn = self.db.begin_write()?;
        let removed = {
            let mut table = txn.open_table(BLOB_INDEX)?;
            let key = chunk_hash.to_bytes();
            let guard = table.remove(key.as_slice())?;
            guard.is_some()
        };
        txn.commit()?;
        Ok(removed)
    }

    /// Insert or replace a `FileRef` for the given file hash, and
    /// update the path index for all paths in the `FileRef`.
    ///
    /// This is the write-path counterpart to `get_fileref`. It
    /// updates both the file_index and path_index tables in a single
    /// transaction.
    pub fn put_fileref(&self, file_hash: &Hash, fileref: &FileRef) -> Result<(), BluError> {
        let txn = self.db.begin_write()?;
        {
            let mut file_table = txn.open_table(FILE_INDEX)?;
            let mut path_table = txn.open_table(PATH_INDEX)?;
            let key = file_hash.to_bytes();
            let value = serialize_cbor(fileref)?;
            file_table.insert(key.as_slice(), value.as_slice())?;
            for path in &fileref.paths {
                let path_str = path.to_string_lossy();
                path_table.insert(path_str.as_ref(), key.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Remove a file from both the file_index and path_index tables.
    ///
    /// Removes all path entries that map to the given file_hash, and
    /// the file_hash entry itself. Does not touch block_index or
    /// blob_index; the caller is responsible for the delete cascade.
    pub fn delete_file(&self, file_hash: &Hash) -> Result<(), BluError> {
        let txn = self.db.begin_write()?;
        {
            let mut file_table = txn.open_table(FILE_INDEX)?;
            let mut path_table = txn.open_table(PATH_INDEX)?;
            let key = file_hash.to_bytes();

            // Remove all path entries pointing to this file_hash.
            // We need to scan for them since path_index maps path ->
            // file_hash, and we don't have a reverse index.
            // Collect paths to remove first to avoid holding a read
            // iterator while mutating.
            let paths_to_remove: Vec<String> = {
                let paths_to_remove: Vec<String> = path_table
                    .iter()?
                    .filter_map(|item| {
                        let (k, v) = item.ok()?;
                        if v.value() == key.as_slice() {
                            Some(k.value().to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                paths_to_remove
            };
            for path in paths_to_remove {
                path_table.remove(path.as_str())?;
            }

            file_table.remove(key.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Atomically write all index updates for one `PutObject` request
    /// under a single redb write transaction.
    ///
    /// Inserts the `FileRef` into `file_index`, the `path -> file_hash`
    /// mapping into `path_index`, new chunk locations into `blob_index`
    /// (dedup-skipped chunks are left untouched), and merges `file_hash
    /// -> position` entries into existing `BlockRef`s in `block_index`,
    /// creating new BlockRefs for chunks not yet present.
    ///
    /// The `new_blob_locations` slice contains only chunks that were
    /// freshly packed into a blob this request; chunks already in the
    /// blob index (dedup hits) must not be re-inserted, so the caller
    /// filters them out before calling.
    ///
    /// The `blockref_updates` slice carries `(chunk_hash, file_hash,
    /// position)` triples for every chunk in the new file, deduped or
    /// not. Each triple is merged into the existing BlockRef for that
    /// chunk_hash (or a fresh `BlockRef` if none exists), then written
    /// back. This keeps the BlockRef's `references` map in lockstep
    /// with the FileRef's chunk list so the delete cascade can find
    /// every file that references a chunk via a single point lookup.
    pub fn put_object(
        &self,
        file_hash: &Hash,
        fileref: &FileRef,
        path: &str,
        new_blob_locations: &[(Hash, BlobBlockLocation)],
        blockref_updates: &[(Hash, Hash, Position)],
    ) -> Result<(), BluError> {
        let txn = self.db.begin_write()?;
        {
            let mut file_table = txn.open_table(FILE_INDEX)?;
            let mut path_table = txn.open_table(PATH_INDEX)?;
            let mut blob_table = txn.open_table(BLOB_INDEX)?;
            let mut block_table = txn.open_table(BLOCK_INDEX)?;

            let file_key = file_hash.to_bytes();
            let file_value = serialize_cbor(fileref)?;
            file_table.insert(file_key.as_slice(), file_value.as_slice())?;
            path_table.insert(path, file_key.as_slice())?;

            for (chunk_hash, location) in new_blob_locations {
                let key = chunk_hash.to_bytes();
                let value = serialize_cbor(location)?;
                blob_table.insert(key.as_slice(), value.as_slice())?;
            }

            // Build a per-chunk_hash accumulator so multiple chunk
            // references in the same file (dedup hits within the file)
            // collapse to a single BlockRef write.
            let mut by_chunk: HashMap<Hash, BlockRef> = HashMap::new();
            for (chunk_hash, file_hash, position) in blockref_updates {
                let blockref = by_chunk.entry(chunk_hash.clone()).or_insert_with(|| {
                    match block_table
                        .get(chunk_hash.to_bytes().as_slice())
                        .ok()
                        .and_then(|g| g)
                    {
                        Some(g) => deserialize_cbor(g.value()).unwrap_or_else(|_| BlockRef::new()),
                        None => BlockRef::new(),
                    }
                });
                blockref
                    .references
                    .insert(file_hash.clone(), position.clone());
            }
            for (chunk_hash, blockref) in &by_chunk {
                let key = chunk_hash.to_bytes();
                let value = serialize_cbor(blockref)?;
                block_table.insert(key.as_slice(), value.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Atomically remove a file and cascade through the block, blob,
    /// and tag indexes, all under a single redb write transaction.
    ///
    /// Removes the FileRef, its path entries, decrements BlockRef
    /// references (dropping chunks that become unreferenced from
    /// blob_index and block_index), and strips the file from any
    /// tag_index entries.
    ///
    /// Dead blob detection: when a chunk is removed from blob_index,
    /// its blob path is recorded. After all chunk removals, the method
    /// scans the surviving blob_index entries for each touched path. A
    /// path with no surviving entries is fully dead and returned in
    /// `DeleteStats::blobs_dead` so the caller can delete the blob file
    /// from the storage backend after the transaction commits.
    ///
    /// Returns [`DeleteStats`] so the caller can log or surface the
    /// work performed. Returns `Ok(DeleteStats::default())` if the
    /// file_hash is not present.
    ///
    /// [`DeleteStats`]: crate::serve::redb_store::DeleteStats
    pub fn delete_object_index(&self, file_hash: &Hash) -> Result<DeleteStats, BluError> {
        let txn = self.db.begin_write()?;
        let stats = {
            let mut file_table = txn.open_table(FILE_INDEX)?;
            let mut path_table = txn.open_table(PATH_INDEX)?;
            let mut blob_table = txn.open_table(BLOB_INDEX)?;
            let mut block_table = txn.open_table(BLOCK_INDEX)?;
            let mut tag_table = txn.open_table(TAG_INDEX)?;

            let file_key = file_hash.to_bytes();

            // Fetch the FileRef so we know which chunks to cascade.
            // If missing, the file was already deleted; treat as a
            // no-op success.
            let fileref: Option<FileRef> = match file_table.get(file_key.as_slice())? {
                Some(g) => deserialize_cbor(g.value()).ok(),
                None => None,
            };
            let fileref = match fileref {
                Some(f) => f,
                None => {
                    return Ok(DeleteStats::default());
                }
            };

            let chunk_hashes: Vec<Hash> = fileref
                .chunkmetas
                .iter()
                .map(|cm| cm.hash.clone())
                .collect();

            // Remove the file and its path entries.
            for path in &fileref.paths {
                let path_str = path.to_string_lossy();
                path_table.remove(path_str.as_ref())?;
            }
            file_table.remove(file_key.as_slice())?;

            let mut stats = DeleteStats::default();

            // Cascade through block_index. For each chunk, remove this
            // file_hash from the BlockRef's references. If empty, the
            // chunk is unreferenced: drop BlockRef and blob_index.
            // Capture the blob path of each removed chunk so we can
            // detect fully-dead blobs after all removals.
            let mut touched_blob_paths: HashSet<PathBuf> = HashSet::new();
            for chunk_hash in &chunk_hashes {
                let key = chunk_hash.to_bytes();
                let existing: Option<BlockRef> = match block_table.get(key.as_slice())? {
                    Some(g) => deserialize_cbor(g.value()).ok(),
                    None => None,
                };
                let mut blockref = match existing {
                    Some(b) => b,
                    None => continue,
                };
                let was_present = blockref.references.remove(file_hash).is_some();
                let _ = was_present;
                if blockref.references.is_empty() {
                    block_table.remove(key.as_slice())?;

                    // Capture the blob path before removing the entry
                    // so we can check for dead blobs afterwards.
                    if let Some(blob_guard) = blob_table.get(key.as_slice())? {
                        let location: BlobBlockLocation = deserialize_cbor(blob_guard.value())?;
                        touched_blob_paths.insert(location.blob_path().clone());
                    }
                    blob_table.remove(key.as_slice())?;
                    stats.blocks_removed += 1;
                    stats.chunks_removed += 1;
                } else {
                    let value = serialize_cbor(&blockref)?;
                    block_table.insert(key.as_slice(), value.as_slice())?;
                }
            }

            // Detect fully-dead blobs. For each touched blob path, scan
            // the surviving blob_index for any entry whose location
            // references that path. If none survive, the blob is dead
            // and the caller should delete it from the backend.
            if !touched_blob_paths.is_empty() {
                // Build the set of blob paths that still have at least
                // one surviving chunk in blob_index.
                let mut live_blob_paths: HashSet<PathBuf> = HashSet::new();
                for item in blob_table.iter()? {
                    let (_k, v) = item?;
                    let location: BlobBlockLocation = deserialize_cbor(v.value())?;
                    live_blob_paths.insert(location.blob_path().clone());
                }
                for path in &touched_blob_paths {
                    if !live_blob_paths.contains(path) {
                        stats.blobs_dead.push(path.clone());
                    }
                }
            }

            // Cascade through tag_index. Remove this file_hash from
            // every tag whose value contains it; drop tags that become
            // empty.
            let tags_to_update: Vec<(String, Vec<Hash>)> = {
                let mut hits = Vec::new();
                for item in tag_table.iter()? {
                    let (k, v) = item?;
                    let tag_name = k.value().to_string();
                    let hashes_set: HashSet<Hash> = deserialize_tag_value(v.value())?;
                    if hashes_set.iter().any(|h| h == file_hash) {
                        let mut hashes: Vec<Hash> = hashes_set.into_iter().collect();
                        hashes.retain(|h| h != file_hash);
                        hits.push((tag_name, hashes));
                    }
                }
                hits
            };
            for (tag_name, remaining) in tags_to_update {
                if remaining.is_empty() {
                    tag_table.remove(tag_name.as_str())?;
                } else {
                    let value = serialize_tag_value(&remaining)?;
                    tag_table.insert(tag_name.as_str(), value.as_slice())?;
                }
                stats.tags_touched += 1;
            }

            stats
        };
        txn.commit()?;
        Ok(stats)
    }

    /// Remove a path from the path_index table. Returns `true` if an
    /// entry was removed.
    pub fn delete_path(&self, path: &str) -> Result<bool, BluError> {
        let txn = self.db.begin_write()?;
        let removed = {
            let mut table = txn.open_table(PATH_INDEX)?;
            let guard = table.remove(path)?;
            guard.is_some()
        };
        txn.commit()?;
        Ok(removed)
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

    /// Count the number of entries in the block index.
    pub fn block_count(&self) -> Result<u64, BluError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(BLOCK_INDEX)?;
        Ok(table.len()?)
    }

    /// Dump all redb tables back into in-memory index structs.
    ///
    /// This is the reverse of [`populate_from_indexes`]. It reads
    /// every entry from all five tables and reconstructs
    /// `PlainIndex`, `BlobIndex`, and `TagIndex`. Used by the index
    /// flush strategy to serialize redb state to encrypted CBOR and
    /// push to the backend.
    ///
    /// The returned `PlainIndex` has `updated_at` set to the current
    /// time, and `created_at` copied from the existing index if
    /// available (otherwise also set to now).
    pub fn dump_to_indexes(&self) -> Result<(PlainIndex, BlobIndex, TagIndex), BluError> {
        let txn = self.db.begin_read()?;

        // Dump file_index -> PlainIndex.files
        let mut files: HashMap<Hash, FileRef> = HashMap::new();
        {
            let file_table = txn.open_table(FILE_INDEX)?;
            for item in file_table.iter()? {
                let (key_guard, value_guard) = item?;
                let file_hash = Hash::from(key_guard.value());
                let fileref: FileRef = deserialize_cbor(value_guard.value())?;
                files.insert(file_hash, fileref);
            }
        }

        // Dump block_index -> PlainIndex.blocks
        let mut blocks: HashMap<Hash, BlockRef> = HashMap::new();
        {
            let block_table = txn.open_table(BLOCK_INDEX)?;
            for item in block_table.iter()? {
                let (key_guard, value_guard) = item?;
                let chunk_hash = Hash::from(key_guard.value());
                let blockref: BlockRef = deserialize_cbor(value_guard.value())?;
                blocks.insert(chunk_hash, blockref);
            }
        }

        // Dump blob_index -> BlobIndex
        let mut blob_index = BlobIndex::new();
        {
            let blob_table = txn.open_table(BLOB_INDEX)?;
            for item in blob_table.iter()? {
                let (key_guard, value_guard) = item?;
                let chunk_hash = Hash::from(key_guard.value());
                let location: BlobBlockLocation = deserialize_cbor(value_guard.value())?;
                blob_index.add_chunk_location(&chunk_hash, &location);
            }
        }

        // Dump tag_index -> TagIndex
        let mut tag_index = TagIndex::new();
        {
            let tag_table = txn.open_table(TAG_INDEX)?;
            for item in tag_table.iter()? {
                let (key_guard, value_guard) = item?;
                let tag_name = key_guard.value().to_string();
                let file_hashes = deserialize_tag_value(value_guard.value())?;
                for fh in &file_hashes {
                    tag_index.add_tag(fh, &tag_name);
                }
            }
        }

        // Build PlainIndex starting from new_empty() (which sets
        // version, created_at, updated_at), then overwrite files and
        // blocks with the dumped data and refresh updated_at.
        let mut plain = PlainIndex::new_empty();
        plain.files = files;
        plain.blocks = blocks;
        plain.updated_at = chrono::Utc::now().naive_utc().with_nanosecond(0).unwrap();

        Ok((plain, blob_index, tag_index))
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
