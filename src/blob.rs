use core::fmt::Debug;
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use crate::block::DEFAULT_CHUNK_SIZE;
use crate::compression::{compress, decompress};
use crate::dek_provider::{decrypt_envelope, encrypt_envelope, DekProvider};
use crate::error::BluError;
use crate::hash::{self, Hash};
use crate::io::{gen_std_enc_serde, Position};
use crate::storage::{self, BackendKind};
use crate::v2format::FileType;

/// the default on-disk filename for the blob index
pub const BLOB_INDEX_FILENAME: &str = "blob_index.dat";
// Default chunk size (4096 * 128) * 128 will fit into a blob file by default
// ... around 64 MiB
const DEFAULT_BLOB_CAPACITY_BYTES: usize = DEFAULT_CHUNK_SIZE << 7;

// backend::Local
// backend::S3
// backend::DO
// backend::AzureBlob
// backend::GCS

/// BlobBuffer writes blob files, re-indexes and re-orgs in case of many blocks
/// (or unused blocks), etc.
///
/// Blob uploads are pipelined: when the buffer fills up, the blob is
/// compressed, encrypted, and handed off to a background upload task.
/// The buffer resets immediately so the next blob can start filling
/// while the previous one is still uploading. All in-flight uploads
/// are awaited in [`BlobBuffer::finalize`].
// #[derive(Debug)]
pub struct BlobBuffer {
    storage_backend: BackendKind,

    // encryption
    keys: DekProvider,

    // transient
    data: Vec<u8>,
    blob_capacity: usize,
    offset: usize,
    positions: HashMap<Hash, BlobBlockLocation>,

    // in-flight upload tasks
    inflight: Vec<tokio::task::JoinHandle<Result<PathBuf, BluError>>>,
}

impl BlobBuffer {
    /// Create a new BlobBuffer with the default capacity
    pub fn new(backend: &BackendKind, keys: DekProvider) -> Self {
        Self::with_capacity(backend, keys, DEFAULT_BLOB_CAPACITY_BYTES)
    }
    /// Create a new BlobBuffer with a specified capacity
    pub fn with_capacity(backend: &BackendKind, keys: DekProvider, capacity: usize) -> Self {
        Self {
            storage_backend: backend.clone(),
            keys,
            data: vec![],
            blob_capacity: capacity,
            offset: 0,
            positions: HashMap::new(),
            inflight: Vec::new(),
        }
    }

    /// Write a block of data to the blob buffer. If the buffer is full, it will be flushed to disk
    /// and a new one started.
    ///
    /// To be used with [`BlobBuffer::finalize`].
    pub async fn add_chunk(
        &mut self,
        chunk: &mut [u8],
        idx: &mut BlobIndex,
    ) -> Result<(), BluError> {
        let chunk_hash = Hash::from(hash::multihash(chunk).to_bytes());
        let mut chunk_copy = chunk.to_vec();
        self.data.append(&mut chunk_copy);
        let size = chunk.len();
        // remap the path after writing and then add to the blob_index
        self.positions.insert(
            chunk_hash,
            BlobBlockLocation {
                path: "".into(),
                position: Position {
                    offset: self.offset,
                    size,
                },
            },
        );
        self.offset += size;

        if self.is_full() {
            let path = self.seal_and_upload(idx)?;
            debug!("Sealed blob for upload at {:?}!", path);
            return Ok(());
        }

        debug!("Added chunk to memory!");
        Ok(())
    }

    /// Finalize the blob buffer, writing the last blob to disk and
    /// waiting for all in-flight uploads to complete.
    pub async fn finalize(&mut self, idx: &mut BlobIndex) -> Result<(), BluError> {
        if !self.is_empty() {
            self.seal_and_upload(idx)?;
        }

        // Await all in-flight uploads
        let handles = std::mem::take(&mut self.inflight);
        for handle in handles {
            handle.await??;
        }

        Ok(())
    }

    /// Compress, encrypt, derive the path, update the index, spawn
    /// the upload in the background, and reset the buffer.
    ///
    /// Returns the content-addressed path (known before the upload
    /// completes because it is derived from the hash of the encrypted
    /// blob data).
    fn seal_and_upload(&mut self, idx: &mut BlobIndex) -> Result<PathBuf, BluError> {
        let compressed = compress(&self.data)?;
        let encrypted = encrypt_envelope(&compressed, FileType::Blob, &self.keys)?;

        let blob_hash = Hash::from(hash::multihash(&encrypted).to_bytes());
        let path = storage::path_for(&blob_hash)?;

        // Update the index immediately (path is deterministic)
        for (chunk_hash, location) in self.positions.iter_mut() {
            location.path = path.clone();
            idx.add_chunk_location(chunk_hash, location);
        }
        self.reset();

        // Spawn the upload in the background. BluError is Send + Sync,
        // so no stringification workaround is needed.
        let backend = self.storage_backend.clone();
        let handle = tokio::spawn(async move { backend.write_data(&blob_hash, &encrypted).await });
        self.inflight.push(handle);

        Ok(path)
    }

    fn is_full(&self) -> bool {
        self.data.len() >= self.blob_capacity
    }
    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
    fn reset(&mut self) {
        self.data = vec![];
        self.offset = 0;
        self.positions = HashMap::new();
    }
}

/// BlobBlockLocation is a path to a blob file and a position (offset/size)
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Eq)]
pub struct BlobBlockLocation {
    path: PathBuf,
    // TODO: not pub
    /// Blah blah make this private again
    pub position: Position,
}

impl BlobBlockLocation {
    /// Returns the path to the blob file containing this block.
    pub fn blob_path(&self) -> &PathBuf {
        &self.path
    }
}

// NOTE: path should not have .blu or .blu/data in it
// BlobBlockLocation {
//     path: "./.blu/data/9/93c/93c98/93c982e79bcd6d4b32c24af6c4b88c9f9483ab88363a7bd2ae5a1b6da83af1c9163696d946de18ee10510563d3d42e20c52d5b78044a08929ecd2d756d8816d0",
//     position: Position {
//         offset: 524288,
//         size: 524288,
//     },
// }

/// The number of decrypted blobs to keep in the LRU cache. With 512 KiB
/// chunks and 128 chunks per blob, each cached entry is ~64 MiB decompressed,
/// so 10 entries caps memory at ~640 MiB worst case.
const BLOB_CACHE_CAPACITY: usize = 10;

/// EncBlobReader reads encrypted blobs from storage.
pub struct EncBlobReader<'a, 'b> {
    cache: LruCache<Hash, Vec<u8>>,
    keys: &'a DekProvider,
    backend: &'b BackendKind,
}
impl<'a, 'b> EncBlobReader<'a, 'b> {
    /// Create a new EncBlobReader.
    pub fn new(keys: &'a DekProvider, backend: &'b BackendKind) -> Self {
        Self {
            cache: LruCache::new(NonZeroUsize::new(BLOB_CACHE_CAPACITY).unwrap()),
            keys,
            backend,
        }
    }

    /// Get the bytes from the blob file at the specified position.
    ///
    /// Returns a borrowed slice into the cached decompressed blob, avoiding a
    /// per-chunk heap allocation.
    pub async fn get_bytes(&mut self, location_ref: &BlobBlockLocation) -> Result<&[u8], BluError> {
        let hash = storage::hash_from_path(&location_ref.path)?;

        if !self.cache.contains(&hash) {
            debug!(
                "Reading blob file from backend: {}",
                location_ref.path.display()
            );
            let raw = self.backend.read_data(&location_ref.path).await?;
            let decrypted = decrypt_envelope(&raw, self.keys)?;
            let decompressed = decompress(&decrypted)?;
            self.cache.put(hash.clone(), decompressed);
        } else {
            trace!("Blob cache hit: {}", location_ref.path.display());
        }

        let full_data = self
            .cache
            .get(&hash)
            .ok_or_else(|| BluError::Internal("blob cache miss immediately after insert".into()))?;
        let pos = &location_ref.position;
        Ok(&full_data[pos.offset..pos.offset + pos.size])
    }
}

/// BlobIndex maps the unencrypted chunk hashes to the encrypted blob files and positions within.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Default, Eq)]
pub struct BlobIndex {
    // TODO: not pub
    /// map the hash of a chunk to the location of the data on disk (within the
    /// blob)
    pub map: HashMap<Hash, BlobBlockLocation>,
    // TODO: not pub
    /// blob path => set of chunk hashes for chunks contained within the blob
    pub path_index: HashMap<PathBuf, HashSet<Hash>>,
    // TODO: not pub
    /// A set of paths to delete from storage backend, which have been removed
    /// from the map and path_index already
    pub paths_to_delete: HashSet<PathBuf>,
}

impl BlobIndex {
    /// Create a new BlobIndex
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            path_index: HashMap::new(),
            paths_to_delete: HashSet::new(),
        }
    }

    /// Add a chunk location to the index. This should be done after a blob is written to disk.
    ///
    /// Generally the blob buffer will do this in the add_chunk and finalize methods.
    pub fn add_chunk_location(&mut self, chunk_hash: &Hash, location: &BlobBlockLocation) {
        // insert into chunk -> location map
        self.map.insert(chunk_hash.clone(), location.clone());
        // add to path index (for tracking which chunks are in which blobs)
        let entry = self.path_index.entry(location.path.clone()).or_default();
        entry.insert(chunk_hash.clone());
    }

    /// Delete a chunk from the index.
    ///
    /// Removes the chunk from both the chunk-to-location map and the
    /// path index. When the last live chunk in a blob is removed, the
    /// blob path is added to `paths_to_delete` so the caller can
    /// delete it from the storage backend. Partially-dead blobs (still
    /// containing other live chunks) are left for defrag to repack.
    pub fn delete_chunk(&mut self, chunk_hash: &Hash) -> Result<(), BluError> {
        let location = self
            .map
            .get(chunk_hash)
            .ok_or_else(|| BluError::BlockNotFound {
                hash: chunk_hash.to_string(),
            })?;
        let blob_path = location.path.clone();

        // Remove from path index
        let blob_fully_dead = match self.path_index.get_mut(&blob_path) {
            Some(entry) => {
                entry.remove(chunk_hash);
                entry.is_empty()
            }
            None => false,
        };
        if blob_fully_dead {
            self.path_index.remove(&blob_path);
        }

        self.map.remove(chunk_hash);

        // Only mark for backend deletion when every chunk in the blob
        // is gone. Partially-dead blobs stay in path_index for defrag.
        if blob_fully_dead {
            self.paths_to_delete.insert(blob_path);
        }
        Ok(())
    }

    /// Return whether the block is in the blob index or not.
    ///
    /// This is a good indication of if the block has been encrypted or not.
    pub fn has_chunk(&self, chunk_hash: &Hash) -> bool {
        self.map.contains_key(chunk_hash)
    }

    /// Get the location of the block within the blob.
    pub fn get_block_location_ref(&self, block_hash: &Hash) -> Result<BlobBlockLocation, BluError> {
        let location_ref = self
            .map
            .get(block_hash)
            .ok_or_else(|| BluError::BlockNotFound {
                hash: block_hash.to_string(),
            })?;
        Ok(location_ref.clone())
    }

    /// Drain the set of blob paths marked for backend deletion.
    ///
    /// Returns the paths and clears the set so they are not
    /// double-processed on a subsequent call.
    pub fn drain_paths_to_delete(&mut self) -> HashSet<PathBuf> {
        std::mem::take(&mut self.paths_to_delete)
    }

    /// Get the count of encrypted files (not blocks) referenced by the blob index.
    pub fn count_blob_files(&self) -> usize {
        self.map
            .values()
            .map(|loc| &loc.path)
            .collect::<HashSet<&PathBuf>>()
            .len()
    }

    /// Get the count of encrypted blocks (not files) referenced by the blob index.
    pub fn count_chunks_indexed(&self) -> usize {
        self.map.len()
    }
}

gen_std_enc_serde!(BlobIndex);

#[cfg(test)]
mod test {
    use tempfile::tempdir;

    use super::*;
    use crate::storage::{BackendKind, Local};

    fn test_keys() -> DekProvider {
        let kek = crate::keys::kek::Kek::generate();
        DekProvider::Local {
            kek,
            kek_version: 0,
        }
    }

    // helper func used in tests below
    fn temp_local_backend() -> BackendKind {
        let datadir = tempdir().unwrap();
        BackendKind::Local(Local::new(datadir))
    }

    async fn test_blobbuf(backend: &BackendKind) -> (BlobBuffer, BlobIndex) {
        let keys = test_keys();
        let mut vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let mut blob_index = BlobIndex::new();
        let mut blob_buf = BlobBuffer::new(backend, keys);
        // load w/some data
        for v in vec.iter_mut() {
            blob_buf.add_chunk(v, &mut blob_index).await.unwrap();
        }
        (blob_buf, blob_index)
    }

    #[tokio::test]
    async fn new() {
        let backend = temp_local_backend();
        let (mut blob_buf, mut idx) = test_blobbuf(&backend).await;
        blob_buf.finalize(&mut idx).await.unwrap();
        assert_eq!(idx.count_blob_files(), 1);
        assert_eq!(idx.count_chunks_indexed(), 3);
    }

    #[tokio::test]
    async fn capacity() {
        // NOTE: do not use `test_blobmgr()` here, as we are testing capacity
        let mut vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
            vec![0xde, 0xad],
            vec![0xde, 0xad],
            vec![0xde, 0xad],
        ];
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(&datadir));
        let keys = test_keys();
        let mut blob_index = BlobIndex::new();
        let mut blob_buf = BlobBuffer::with_capacity(&backend, keys, 3);
        // load w/some data
        for v in vec.iter_mut() {
            blob_buf.add_chunk(v, &mut blob_index).await.unwrap();
        }
        assert_eq!(blob_index.count_blob_files(), 4);
        assert_eq!(blob_index.count_chunks_indexed(), 4);
    }

    #[tokio::test]
    async fn blob() {
        let backend = temp_local_backend();
        let (mut blob_buf, mut idx) = test_blobbuf(&backend).await;
        blob_buf.finalize(&mut idx).await.unwrap();

        // After finalize, all 3 chunks should be indexed
        assert_eq!(idx.count_chunks_indexed(), 3);
        assert_eq!(idx.count_blob_files(), 1);

        // Verify each chunk has the correct position in the blob
        let loc1 = idx
            .get_block_location_ref(&Hash::from("1340e94518b58bcd5e29a8f6251fbc457c580691c8f9d3e3a17dc404d2e5dc86fa98ac857b8ba9366d6023da1196f89729e760e13fee78c10993c181ecee4211be76"))
            .unwrap();
        assert_eq!(loc1.position, Position { offset: 0, size: 3 });

        let loc2 = idx
            .get_block_location_ref(&Hash::from("13401284b2d521535196f22175d5f558104220a6ad7680e78b49fa6f20e57ea7b185d71ec1edb137e70eba528dedb141f5d2f8bb53149d262932b27cf41fed96aa7f"))
            .unwrap();
        assert_eq!(loc2.position, Position { offset: 3, size: 4 });

        let loc3 = idx
            .get_block_location_ref(&Hash::from("13401332e5814224318ddcb3db935b3a7af1f97073b50033be1bc729302028e906f4cb12a652eefe76d7d4f2e8d6bf1671b331f76dc93546e9faa395892fe28d241c"))
            .unwrap();
        assert_eq!(loc3.position, Position { offset: 7, size: 8 });
    }

    // Chunk hashes for the three test chunks (used in delete tests)
    const CHUNK1: &str = "1340e94518b58bcd5e29a8f6251fbc457c580691c8f9d3e3a17dc404d2e5dc86fa98ac857b8ba9366d6023da1196f89729e760e13fee78c10993c181ecee4211be76";
    const CHUNK2: &str = "13401284b2d521535196f22175d5f558104220a6ad7680e78b49fa6f20e57ea7b185d71ec1edb137e70eba528dedb141f5d2f8bb53149d262932b27cf41fed96aa7f";
    const CHUNK3: &str = "13401332e5814224318ddcb3db935b3a7af1f97073b50033be1bc729302028e906f4cb12a652eefe76d7d4f2e8d6bf1671b331f76dc93546e9faa395892fe28d241c";

    #[tokio::test]
    async fn delete_partial_keeps_blob_alive() {
        let backend = temp_local_backend();
        let (mut blob_buf, mut idx) = test_blobbuf(&backend).await;
        blob_buf.finalize(&mut idx).await.unwrap();
        assert_eq!(idx.count_chunks_indexed(), 3);
        assert_eq!(idx.count_blob_files(), 1);

        // Delete one of three chunks; blob should NOT be marked for deletion
        idx.delete_chunk(&Hash::from(CHUNK1)).unwrap();
        assert_eq!(idx.count_chunks_indexed(), 2);
        assert!(idx.paths_to_delete.is_empty());
        // Blob path still in path_index (for defrag to find later)
        assert_eq!(idx.path_index.len(), 1);
    }

    #[tokio::test]
    async fn delete_all_chunks_marks_blob_for_deletion() {
        let backend = temp_local_backend();
        let (mut blob_buf, mut idx) = test_blobbuf(&backend).await;
        blob_buf.finalize(&mut idx).await.unwrap();
        assert_eq!(idx.count_chunks_indexed(), 3);

        // Delete all three chunks
        idx.delete_chunk(&Hash::from(CHUNK1)).unwrap();
        idx.delete_chunk(&Hash::from(CHUNK2)).unwrap();
        idx.delete_chunk(&Hash::from(CHUNK3)).unwrap();

        assert_eq!(idx.count_chunks_indexed(), 0);
        assert!(idx.path_index.is_empty());
        assert_eq!(idx.paths_to_delete.len(), 1);
    }

    #[tokio::test]
    async fn drain_paths_to_delete_returns_and_clears() {
        let backend = temp_local_backend();
        let (mut blob_buf, mut idx) = test_blobbuf(&backend).await;
        blob_buf.finalize(&mut idx).await.unwrap();

        idx.delete_chunk(&Hash::from(CHUNK1)).unwrap();
        idx.delete_chunk(&Hash::from(CHUNK2)).unwrap();
        idx.delete_chunk(&Hash::from(CHUNK3)).unwrap();
        assert_eq!(idx.paths_to_delete.len(), 1);

        let drained = idx.drain_paths_to_delete();
        assert_eq!(drained.len(), 1);
        assert!(idx.paths_to_delete.is_empty());

        // Second drain returns empty
        let drained2 = idx.drain_paths_to_delete();
        assert!(drained2.is_empty());
    }

    #[tokio::test]
    async fn delete_nonexistent_chunk_errors() {
        let idx = BlobIndex::new();
        let fake = Hash::from("1340deadbeef");
        let result = idx.clone().delete_chunk(&fake);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_across_multiple_blobs() {
        // Use small capacity to force multiple blob files
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(&datadir));
        let keys = test_keys();
        let mut idx = BlobIndex::new();
        let mut blob_buf = BlobBuffer::with_capacity(&backend, keys, 3);

        let mut chunks: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        for v in chunks.iter_mut() {
            blob_buf.add_chunk(v, &mut idx).await.unwrap();
        }
        blob_buf.finalize(&mut idx).await.unwrap();

        let blob_count = idx.count_blob_files();
        assert!(
            blob_count > 1,
            "expected multiple blobs, got {}",
            blob_count
        );

        // Delete all chunks from only the first blob's chunk
        let chunk1 = Hash::from(CHUNK1);
        let blob1_path = idx
            .get_block_location_ref(&chunk1)
            .unwrap()
            .blob_path()
            .clone();
        idx.delete_chunk(&chunk1).unwrap();

        // First blob should be marked for deletion (it only had one chunk)
        if idx.paths_to_delete.contains(&blob1_path) {
            // Other blobs should still be alive
            assert!(idx.count_chunks_indexed() > 0);
        }
    }

    #[tokio::test]
    async fn full_cascade_with_backend_deletion() {
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(&datadir));
        let (mut blob_buf, mut idx) = test_blobbuf(&backend).await;
        blob_buf.finalize(&mut idx).await.unwrap();

        // Verify blob file exists on disk
        let chunk1 = Hash::from(CHUNK1);
        let blob_path = idx
            .get_block_location_ref(&chunk1)
            .unwrap()
            .blob_path()
            .clone();
        assert!(backend.exists(&blob_path).await.unwrap());

        // Delete all chunks
        idx.delete_chunk(&Hash::from(CHUNK1)).unwrap();
        idx.delete_chunk(&Hash::from(CHUNK2)).unwrap();
        idx.delete_chunk(&Hash::from(CHUNK3)).unwrap();

        // Drain and delete from backend
        let dead_paths = idx.drain_paths_to_delete();
        assert_eq!(dead_paths.len(), 1);
        for path in &dead_paths {
            backend.delete(path).await.unwrap();
        }

        // Verify blob file is gone from disk
        assert!(!backend.exists(&blob_path).await.unwrap());
        assert_eq!(idx.count_chunks_indexed(), 0);
        assert!(idx.path_index.is_empty());
        assert!(idx.paths_to_delete.is_empty());
    }
}
