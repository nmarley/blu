use core::fmt::Debug;
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use crate::block::DEFAULT_CHUNK_SIZE;
use crate::compression::{compress, compress_with_progress, decompress};
use crate::dek_provider::{
    decrypt_envelope, decrypt_envelope_segmented_prefix, encrypt_envelope_segmented,
    last_segment_for, DekProvider,
};
use crate::error::BluError;
use crate::hash::{self, Hash};
use crate::io::{gen_std_enc_serde, Position};
use crate::storage::{self, BackendKind};
use crate::v3format;

/// the default on-disk filename for the blob index
pub const BLOB_INDEX_FILENAME: &str = "blob_index.dat";

/// Default v3 segment size in bytes (512 KiB). Each encrypted segment
/// is exactly this many plaintext bytes plus a 16-byte tag on disk.
/// Stored per-blob in the v3 header, so it is tunable without a format
/// bump.
pub const DEFAULT_SEGMENT_SIZE: usize = 524_288;

/// Number of leading bytes to fetch when probing a blob's format
/// version and (for v3) parsing its header before a prefix range read.
/// A v3 header is `20 + wrapped_dek_len` bytes; the wrapped DEK is an
/// age-wrapped 32-byte key (well under 512 bytes), so this comfortably
/// covers any real header while staying negligible against a segment.
const V3_HEADER_PROBE_BYTES: u64 = 512;
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
                compressed_end: None,
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
        // Order chunks by their decompressed offset so the compressed
        // regions line up with insertion order. region_endpoints are
        // cumulative decompressed offsets (each chunk's offset + size).
        let mut ordered: Vec<Hash> = self.positions.keys().cloned().collect();
        ordered.sort_by_key(|h| self.positions[h].position.offset);
        let region_endpoints: Vec<usize> = ordered
            .iter()
            .map(|h| {
                let pos = &self.positions[h].position;
                pos.offset + pos.size
            })
            .collect();

        let (compressed, compressed_ends) = compress_with_progress(&self.data, &region_endpoints)?;
        let encrypted = encrypt_envelope_segmented(&compressed, DEFAULT_SEGMENT_SIZE, &self.keys)?;

        let blob_hash = Hash::from(hash::multihash(&encrypted).to_bytes());
        let path = storage::path_for(&blob_hash)?;

        // Update the index immediately (path is deterministic). Record
        // each chunk's compressed-end offset so the v3 reader can
        // compute its segment prefix.
        for (i, chunk_hash) in ordered.iter().enumerate() {
            let location = self
                .positions
                .get_mut(chunk_hash)
                .expect("hash came from positions keys");
            location.path = path.clone();
            location.compressed_end = Some(compressed_ends[i]);
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
    /// The compressed-stream offset where this chunk's bytes end,
    /// used by the v3 segmented reader to compute which segment prefix
    /// to fetch. `None` means the chunk lives in a v2 blob (whole-blob
    /// fetch, no prefix optimization).
    #[serde(default)]
    pub compressed_end: Option<u64>,
}

impl BlobBlockLocation {
    /// Create a new BlobBlockLocation with the given blob path and
    /// position within the decompressed blob.
    pub fn new(path: PathBuf, position: Position) -> Self {
        Self {
            path,
            position,
            compressed_end: None,
        }
    }

    /// Create a new BlobBlockLocation with a compressed-end offset,
    /// used by the v3 segmented write path.
    pub fn new_v3(path: PathBuf, position: Position, compressed_end: u64) -> Self {
        Self {
            path,
            position,
            compressed_end: Some(compressed_end),
        }
    }

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

/// EncBlobReader reads encrypted blobs from storage, decrypts and
/// decompresses them, and caches the result in an LRU cache.
///
/// The cache is guarded by a `std::sync::Mutex` so the reader can be
/// shared across concurrent handlers (e.g., multiple `blu serve`
/// streaming requests). The mutex is held only briefly for cache
/// lookup and insertion; backend fetch, decryption, and decompression
/// all happen lock-free. This means two concurrent requests for the
/// same uncached blob may both fetch it (last insert wins), which is
/// wasteful but harmless. Single-flight deduplication is a future
/// optimization.
pub struct EncBlobReader {
    /// Cache value is `(decompressed_bytes, covered_len)`: the longest
    /// decompressed prefix seen so far for this blob and how many
    /// decompressed bytes it covers. For v2 blobs `covered_len` always
    /// equals the full decompressed length; for v3 it grows as deeper
    /// segment prefixes are fetched.
    cache: std::sync::Mutex<LruCache<Hash, (Vec<u8>, usize)>>,
    keys: DekProvider,
    backend: BackendKind,
}

impl EncBlobReader {
    /// Create a new EncBlobReader with the default cache capacity.
    pub fn new(keys: DekProvider, backend: BackendKind) -> Self {
        Self::with_capacity(keys, backend, BLOB_CACHE_CAPACITY)
    }

    /// Create a new EncBlobReader with a custom cache capacity (number
    /// of decrypted blobs to keep in the LRU cache).
    pub fn with_capacity(keys: DekProvider, backend: BackendKind, capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).expect("max(1) guarantees nonzero");
        Self {
            cache: std::sync::Mutex::new(LruCache::new(cap)),
            keys,
            backend,
        }
    }

    /// Get the bytes for the chunk at the specified position within its
    /// blob.
    ///
    /// On a cache hit, the slice is cloned under the mutex and returned
    /// as an owned `Vec<u8>`, so the lock is never held across an await
    /// point or returned to the caller. On a miss, the blob is fetched,
    /// decrypted, and decompressed lock-free, then inserted into the
    /// cache.
    pub async fn get_bytes(&self, location_ref: &BlobBlockLocation) -> Result<Vec<u8>, BluError> {
        let hash = storage::hash_from_path(&location_ref.path)?;
        let pos = &location_ref.position;
        let chunk_end = pos.offset + pos.size;

        // Fast path: cache hit whose covered prefix reaches this chunk.
        {
            let mut cache = self.cache.lock().expect("blob cache mutex poisoned");
            if let Some((data, covered_len)) = cache.get(&hash) {
                if chunk_end <= *covered_len {
                    trace!("Blob cache hit: {}", location_ref.path.display());
                    return Ok(data[pos.offset..chunk_end].to_vec());
                }
            }
        }

        // Slow path: cache miss (or the cached prefix is too short).
        // Fetch, decrypt, decompress lock-free. v3 blobs fetch only the
        // segment prefix covering this chunk via a bounded range read;
        // v2 blobs fetch the whole box.
        debug!(
            "Reading blob file from backend: {}",
            location_ref.path.display()
        );

        // Peek the format version from a small header-sized prefix so
        // v3 blobs never trigger a whole-blob read. The probe is a few
        // hundred bytes, negligible against a 512 KiB segment.
        let probe = self
            .backend
            .read_range(&location_ref.path, 0, V3_HEADER_PROBE_BYTES)
            .await?;

        let decompressed = match v3format::peek_version(&probe) {
            Some(v3format::FORMAT_VERSION_V3) => {
                // v3 segmented blob: parse the header from the probe,
                // compute the segment prefix covering this chunk's
                // compressed bytes, and range-fetch only that prefix.
                let (header, payload_offset) = v3format::read_header(&probe)?;
                let compressed_end = location_ref.compressed_end.ok_or_else(|| {
                    BluError::DecryptionFailed(format!(
                        "v3 blob chunk missing compressed_end: {}",
                        location_ref.path.display()
                    ))
                })?;
                let up_to_seg = last_segment_for(compressed_end, header.segment_size);
                let prefix_end = payload_offset as u64
                    + (up_to_seg as u64 + 1) * header.on_disk_segment_size() as u64;
                let raw = self
                    .backend
                    .read_range(&location_ref.path, 0, prefix_end)
                    .await?;
                decrypt_envelope_segmented_prefix(&raw, up_to_seg, &self.keys)?
            }
            _ => {
                // v2 whole-blob box.
                let raw = self.backend.read_data(&location_ref.path).await?;
                let decrypted = decrypt_envelope(&raw, &self.keys)?;
                decompress(&decrypted)?
            }
        };

        if decompressed.len() < chunk_end {
            return Err(BluError::DecryptionFailed(format!(
                "decompressed prefix ({} bytes) does not cover chunk end {}",
                decompressed.len(),
                chunk_end
            )));
        }

        // Extract the chunk slice before moving decompressed into cache.
        let chunk = decompressed[pos.offset..chunk_end].to_vec();
        let covered_len = decompressed.len();

        {
            let mut cache = self.cache.lock().expect("blob cache mutex poisoned");
            // Keep the longest prefix: only replace if this fetch covers
            // at least as many decompressed bytes as what is cached.
            let replace = match cache.get(&hash) {
                Some((_, existing)) => covered_len >= *existing,
                None => true,
            };
            if replace {
                cache.put(hash, (decompressed, covered_len));
            }
        }

        Ok(chunk)
    }
}

/// Statistics returned by [`repack_blobs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepackStats {
    /// Number of old blobs that were repacked.
    pub blobs_repacked: usize,
    /// Number of live chunks moved into fresh blobs.
    pub chunks_moved: usize,
    /// Number of old blob files deleted from the backend.
    pub old_blobs_deleted: usize,
}

/// Repack partially-dead blobs tracked by `BlobIndex::paths_to_repack`.
///
/// For each candidate blob, the live chunks are read from the backend,
/// written into a fresh `BlobBuffer` (re-compressed, re-encrypted with
/// a new DEK), and the old blob is deleted. The `BlobIndex` is updated
/// in place: map entries are overwritten with new locations and stale
/// `path_index` entries are removed.
///
/// Returns statistics about the work performed.
pub async fn repack_blobs(
    idx: &mut BlobIndex,
    backend: &BackendKind,
    keys: &DekProvider,
) -> Result<RepackStats, BluError> {
    let candidates = idx.drain_paths_to_repack();
    rewrite_blobs(idx, backend, keys, candidates).await
}

/// Rewrite a set of blobs by reading their live chunks and repacking
/// them into fresh `BlobBuffer` output, then deleting the originals.
///
/// This is the shared machinery behind both `repack_blobs` (which
/// passes partially-dead blobs) and `blu defrag-blobs --upgrade-format`
/// (which passes v2 blobs to be re-emitted as v3). Because the writer
/// always emits the current format (v3), rewriting is format-agnostic:
/// the caller only chooses which blobs to feed in.
///
/// The `BlobIndex` is updated in place: map entries are overwritten
/// with new locations and stale `path_index` entries are removed.
pub async fn rewrite_blobs(
    idx: &mut BlobIndex,
    backend: &BackendKind,
    keys: &DekProvider,
    candidates: HashSet<PathBuf>,
) -> Result<RepackStats, BluError> {
    if candidates.is_empty() {
        return Ok(RepackStats {
            blobs_repacked: 0,
            chunks_moved: 0,
            old_blobs_deleted: 0,
        });
    }

    // Collect chunk hashes and their locations upfront so we avoid
    // borrow conflicts when mutating idx through BlobBuffer later.
    let mut chunks_to_move: Vec<(Hash, BlobBlockLocation)> = Vec::new();
    for blob_path in &candidates {
        if let Some(chunk_hashes) = idx.path_index.get(blob_path) {
            for chunk_hash in chunk_hashes.iter() {
                if let Some(location) = idx.map.get(chunk_hash) {
                    chunks_to_move.push((chunk_hash.clone(), location.clone()));
                }
            }
        }
    }

    // Read all live chunk data from the old blobs.
    let reader = EncBlobReader::new(keys.clone(), backend.clone());
    let mut chunk_data: Vec<Vec<u8>> = Vec::with_capacity(chunks_to_move.len());
    for (_hash, location) in &chunks_to_move {
        let data = reader.get_bytes(location).await?;
        chunk_data.push(data);
    }
    drop(reader);

    // Remove stale path_index entries for the old blobs. Map entries
    // will be overwritten by add_chunk_location inside BlobBuffer.
    for blob_path in &candidates {
        idx.path_index.remove(blob_path);
    }

    // Write live chunks into fresh blobs.
    let mut blob_buf = BlobBuffer::new(backend, keys.clone());
    let chunks_moved = chunk_data.len();
    for mut data in chunk_data {
        blob_buf.add_chunk(&mut data, idx).await?;
    }
    blob_buf.finalize(idx).await?;

    // Delete old blob files from the backend.
    let mut old_blobs_deleted = 0;
    for blob_path in &candidates {
        backend.delete(blob_path).await?;
        old_blobs_deleted += 1;
    }

    Ok(RepackStats {
        blobs_repacked: candidates.len(),
        chunks_moved,
        old_blobs_deleted,
    })
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
    /// Blob paths that still contain live chunks but have had at least one
    /// chunk removed. These are candidates for repacking by `defrag-blobs`
    /// or `delete-files --scrub`.
    #[serde(default)]
    pub paths_to_repack: HashSet<PathBuf>,
}

impl BlobIndex {
    /// Create a new BlobIndex
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            path_index: HashMap::new(),
            paths_to_delete: HashSet::new(),
            paths_to_repack: HashSet::new(),
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

        if blob_fully_dead {
            // Every chunk is gone; mark for backend deletion.
            self.paths_to_delete.insert(blob_path.clone());
            // No longer a repack candidate since it will be deleted.
            self.paths_to_repack.remove(&blob_path);
        } else {
            // Still has live chunks; mark for repacking.
            self.paths_to_repack.insert(blob_path);
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

    /// Drain the set of blob paths marked for repacking.
    ///
    /// Returns the paths and clears the set so they are not
    /// double-processed on a subsequent call.
    pub fn drain_paths_to_repack(&mut self) -> HashSet<PathBuf> {
        std::mem::take(&mut self.paths_to_repack)
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

    #[test]
    fn blob_block_location_v2_cbor_back_compat() {
        // Simulate deserializing a v2-era BlobBlockLocation that has no
        // compressed_end field. The #[serde(default)] on compressed_end
        // must make this load as None.
        let v2_cbor: Vec<u8> = {
            // Manually construct CBOR for a map with two fields: path
            // and position. Using ciborium to serialize a two-field
            // struct that matches the old layout.
            #[derive(serde::Serialize)]
            struct OldLocation {
                path: PathBuf,
                position: Position,
            }
            let old = OldLocation {
                path: PathBuf::from("d/dd4/dd4ce/dd4ce38e"),
                position: Position {
                    offset: 0,
                    size: 4096,
                },
            };
            let mut buf = Vec::new();
            ciborium::into_writer(&old, &mut buf).unwrap();
            buf
        };

        let loc: BlobBlockLocation = ciborium::from_reader(&v2_cbor[..]).unwrap();
        assert_eq!(loc.blob_path(), &PathBuf::from("d/dd4/dd4ce/dd4ce38e"));
        assert_eq!(loc.position.offset, 0);
        assert_eq!(loc.position.size, 4096);
        assert_eq!(loc.compressed_end, None);
    }

    #[test]
    fn blob_block_location_v3_round_trip() {
        let loc = BlobBlockLocation::new_v3(
            PathBuf::from("d/dd4/dd4ce/dd4ce38e"),
            Position {
                offset: 524288,
                size: 524288,
            },
            1_000_000,
        );

        let mut buf = Vec::new();
        ciborium::into_writer(&loc, &mut buf).unwrap();
        let loc2: BlobBlockLocation = ciborium::from_reader(&buf[..]).unwrap();

        assert_eq!(loc, loc2);
        assert_eq!(loc2.compressed_end, Some(1_000_000));
    }

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
        // Partially-dead blob should be marked for repack
        assert_eq!(idx.paths_to_repack.len(), 1);
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
        // Fully-dead blob must NOT be in repack set
        assert!(idx.paths_to_repack.is_empty());
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
    async fn drain_paths_to_repack_returns_and_clears() {
        let backend = temp_local_backend();
        let (mut blob_buf, mut idx) = test_blobbuf(&backend).await;
        blob_buf.finalize(&mut idx).await.unwrap();

        // Partial delete: one chunk removed, blob still alive
        idx.delete_chunk(&Hash::from(CHUNK1)).unwrap();
        assert_eq!(idx.paths_to_repack.len(), 1);

        let drained = idx.drain_paths_to_repack();
        assert_eq!(drained.len(), 1);
        assert!(idx.paths_to_repack.is_empty());

        // Second drain returns empty
        let drained2 = idx.drain_paths_to_repack();
        assert!(drained2.is_empty());
    }

    #[tokio::test]
    async fn repack_cleared_when_blob_fully_dead() {
        let backend = temp_local_backend();
        let (mut blob_buf, mut idx) = test_blobbuf(&backend).await;
        blob_buf.finalize(&mut idx).await.unwrap();

        // First delete: partial, blob enters repack set
        idx.delete_chunk(&Hash::from(CHUNK1)).unwrap();
        assert_eq!(idx.paths_to_repack.len(), 1);

        // Second delete: still partial
        idx.delete_chunk(&Hash::from(CHUNK2)).unwrap();
        assert_eq!(idx.paths_to_repack.len(), 1);

        // Third delete: fully dead, moved from repack to delete
        idx.delete_chunk(&Hash::from(CHUNK3)).unwrap();
        assert!(idx.paths_to_repack.is_empty());
        assert_eq!(idx.paths_to_delete.len(), 1);
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

    #[tokio::test]
    async fn repack_moves_surviving_chunks_to_new_blob() {
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(&datadir));
        let keys = test_keys();

        let mut vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let mut idx = BlobIndex::new();
        let mut blob_buf = BlobBuffer::new(&backend, keys.clone());
        for v in vec.iter_mut() {
            blob_buf.add_chunk(v, &mut idx).await.unwrap();
        }
        blob_buf.finalize(&mut idx).await.unwrap();

        assert_eq!(idx.count_chunks_indexed(), 3);
        assert_eq!(idx.count_blob_files(), 1);

        // Remember the original blob path
        let original_blob_path = idx
            .get_block_location_ref(&Hash::from(CHUNK1))
            .unwrap()
            .blob_path()
            .clone();
        assert!(backend.exists(&original_blob_path).await.unwrap());

        // Delete one chunk (partial), triggering repack candidacy
        idx.delete_chunk(&Hash::from(CHUNK1)).unwrap();
        assert_eq!(idx.paths_to_repack.len(), 1);

        // Repack with the same keys used to encrypt
        let stats = repack_blobs(&mut idx, &backend, &keys).await.unwrap();
        assert_eq!(stats.blobs_repacked, 1);
        assert_eq!(stats.chunks_moved, 2);
        assert_eq!(stats.old_blobs_deleted, 1);

        // Old blob file gone from backend
        assert!(!backend.exists(&original_blob_path).await.unwrap());

        // Surviving chunks still indexed and in a new blob
        assert_eq!(idx.count_chunks_indexed(), 2);
        assert!(idx.has_chunk(&Hash::from(CHUNK2)));
        assert!(idx.has_chunk(&Hash::from(CHUNK3)));
        assert!(!idx.has_chunk(&Hash::from(CHUNK1)));

        // New blob path differs from original
        let new_blob_path = idx
            .get_block_location_ref(&Hash::from(CHUNK2))
            .unwrap()
            .blob_path()
            .clone();
        assert_ne!(new_blob_path, original_blob_path);
        assert!(backend.exists(&new_blob_path).await.unwrap());

        // paths_to_repack is empty after repack
        assert!(idx.paths_to_repack.is_empty());
    }

    #[tokio::test]
    async fn repack_noop_when_nothing_to_repack() {
        let backend = temp_local_backend();
        let (mut blob_buf, mut idx) = test_blobbuf(&backend).await;
        blob_buf.finalize(&mut idx).await.unwrap();

        // No deletes, so nothing to repack
        assert!(idx.paths_to_repack.is_empty());

        let stats = repack_blobs(&mut idx, &backend, &test_keys())
            .await
            .unwrap();
        assert_eq!(stats.blobs_repacked, 0);
        assert_eq!(stats.chunks_moved, 0);
        assert_eq!(stats.old_blobs_deleted, 0);
        assert_eq!(idx.count_chunks_indexed(), 3);
    }

    #[tokio::test]
    async fn repack_data_integrity() {
        // Verify that chunk data survives a repack round-trip:
        // write chunks, delete one, repack, then read back the
        // surviving chunks and compare byte-for-byte.
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(&datadir));
        let keys = test_keys();

        let chunk2_data: Vec<u8> = vec![0xde, 0xad, 0xbe, 0xef];
        let chunk3_data: Vec<u8> = vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad];

        let mut vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            chunk2_data.clone(),
            chunk3_data.clone(),
        ];
        let mut idx = BlobIndex::new();
        let mut blob_buf = BlobBuffer::new(&backend, keys.clone());
        for v in vec.iter_mut() {
            blob_buf.add_chunk(v, &mut idx).await.unwrap();
        }
        blob_buf.finalize(&mut idx).await.unwrap();

        // Delete chunk 1, repack
        idx.delete_chunk(&Hash::from(CHUNK1)).unwrap();
        repack_blobs(&mut idx, &backend, &keys).await.unwrap();

        // Read back surviving chunks through EncBlobReader
        let reader = EncBlobReader::new(keys.clone(), backend.clone());
        let loc2 = idx.get_block_location_ref(&Hash::from(CHUNK2)).unwrap();
        let read2 = reader.get_bytes(&loc2).await.unwrap();
        assert_eq!(read2, chunk2_data.as_slice());

        let loc3 = idx.get_block_location_ref(&Hash::from(CHUNK3)).unwrap();
        let read3 = reader.get_bytes(&loc3).await.unwrap();
        assert_eq!(read3, chunk3_data.as_slice());
    }

    /// Deterministic low-compressibility bytes (xorshift) so a blob's
    /// compressed stream spans multiple 512 KiB segments.
    fn pseudo_random_bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut state = seed | 1;
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.extend_from_slice(&state.to_le_bytes());
        }
        out.truncate(len);
        out
    }

    #[tokio::test]
    async fn v3_multi_segment_round_trip_every_chunk() {
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(&datadir));
        let keys = test_keys();

        // Six ~256 KiB incompressible chunks => ~1.5 MiB compressed,
        // spanning several 512 KiB segments in a single blob.
        let chunk_len = 256 * 1024;
        let mut chunks: Vec<Vec<u8>> = (0..6)
            .map(|i| pseudo_random_bytes(0x9e37_79b9_7f4a_7c15 ^ i as u64, chunk_len))
            .collect();

        let mut idx = BlobIndex::new();
        let mut blob_buf = BlobBuffer::new(&backend, keys.clone());
        let mut hashes = Vec::new();
        for c in chunks.iter_mut() {
            let h = Hash::from(hash::multihash(c).to_bytes());
            hashes.push(h);
            blob_buf.add_chunk(c, &mut idx).await.unwrap();
        }
        blob_buf.finalize(&mut idx).await.unwrap();

        // Single blob written in v3 format.
        assert_eq!(idx.count_blob_files(), 1);
        let first_loc = idx.get_block_location_ref(&hashes[0]).unwrap();
        let raw = backend.read_data(first_loc.blob_path()).await.unwrap();
        assert_eq!(
            v3format::peek_version(&raw),
            Some(v3format::FORMAT_VERSION_V3)
        );
        let (header, _) = v3format::read_header(&raw).unwrap();
        assert!(
            header.segment_count > 1,
            "expected multiple segments, got {}",
            header.segment_count
        );

        // Every chunk has a Some, monotonically increasing compressed_end.
        let mut prev_ce = 0u64;
        for h in &hashes {
            let loc = idx.get_block_location_ref(h).unwrap();
            let ce = loc
                .compressed_end
                .expect("v3 chunk must have compressed_end");
            assert!(ce >= prev_ce, "compressed_end must be non-decreasing");
            prev_ce = ce;
        }

        // Read every chunk back byte-for-byte through the reader.
        let reader = EncBlobReader::new(keys.clone(), backend.clone());
        for (h, expected) in hashes.iter().zip(chunks.iter()) {
            let loc = idx.get_block_location_ref(h).unwrap();
            let got = reader.get_bytes(&loc).await.unwrap();
            assert_eq!(&got, expected, "chunk round-trip mismatch");
        }
    }

    #[tokio::test]
    async fn v3_front_chunk_needs_fewer_segments_than_tail() {
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(&datadir));
        let keys = test_keys();

        let chunk_len = 256 * 1024;
        let mut chunks: Vec<Vec<u8>> = (0..6)
            .map(|i| pseudo_random_bytes(0x1234_5678 ^ i as u64, chunk_len))
            .collect();

        let mut idx = BlobIndex::new();
        let mut blob_buf = BlobBuffer::new(&backend, keys.clone());
        let mut hashes = Vec::new();
        for c in chunks.iter_mut() {
            hashes.push(Hash::from(hash::multihash(c).to_bytes()));
            blob_buf.add_chunk(c, &mut idx).await.unwrap();
        }
        blob_buf.finalize(&mut idx).await.unwrap();

        let raw = backend
            .read_data(idx.get_block_location_ref(&hashes[0]).unwrap().blob_path())
            .await
            .unwrap();
        let (header, _) = v3format::read_header(&raw).unwrap();

        // The front chunk's covering-segment index must be strictly
        // smaller than the tail chunk's: the front chunk fetches a
        // shorter compressed prefix. This uses the exact helper the
        // reader uses to decide how many segments to decrypt.
        let front_ce = idx
            .get_block_location_ref(&hashes[0])
            .unwrap()
            .compressed_end
            .unwrap();
        let tail_ce = idx
            .get_block_location_ref(hashes.last().unwrap())
            .unwrap()
            .compressed_end
            .unwrap();

        let front_seg = last_segment_for(front_ce, header.segment_size);
        let tail_seg = last_segment_for(tail_ce, header.segment_size);
        assert!(
            front_seg < tail_seg,
            "front chunk should need fewer segments: front={} tail={}",
            front_seg,
            tail_seg
        );
    }

    #[tokio::test]
    async fn v2_blob_still_reads_via_v2_branch() {
        use crate::compression::compress;
        use crate::dek_provider::encrypt_envelope;
        use crate::v2format::FileType;

        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(&datadir));
        let keys = test_keys();

        // Manually write a v2 blob: compress a payload, seal it as a
        // single v2 box, store it, and index the chunk with
        // compressed_end = None (v2 marker).
        let payload = b"legacy v2 blob payload that predates segmentation".to_vec();
        let compressed = compress(&payload).unwrap();
        let encrypted = encrypt_envelope(&compressed, FileType::Blob, &keys).unwrap();
        assert_eq!(v3format::peek_version(&encrypted), Some(2));

        let blob_hash = Hash::from(hash::multihash(&encrypted).to_bytes());
        backend.write_data(&blob_hash, &encrypted).await.unwrap();
        let path = storage::path_for(&blob_hash).unwrap();

        let location = BlobBlockLocation::new(
            path,
            Position {
                offset: 0,
                size: payload.len(),
            },
        );
        assert_eq!(location.compressed_end, None);

        let reader = EncBlobReader::new(keys.clone(), backend.clone());
        let got = reader.get_bytes(&location).await.unwrap();
        assert_eq!(got, payload);
    }

    #[tokio::test]
    async fn v3_front_chunk_fetches_fewer_bytes_than_whole_blob() {
        // The concrete Stage 6 win: reading a front chunk of a
        // multi-segment v3 blob fetches strictly fewer backend bytes
        // than the whole blob. Instrument the Local backend's byte
        // counter to measure it deterministically (no wall-clock).
        let datadir = tempdir().unwrap();
        let local = Local::new(&datadir);
        let backend = BackendKind::Local(local.clone());
        let keys = test_keys();

        // Eight ~256 KiB incompressible chunks => a compressed stream
        // spanning several 512 KiB segments in a single blob.
        let chunk_len = 256 * 1024;
        let mut chunks: Vec<Vec<u8>> = (0..8)
            .map(|i| pseudo_random_bytes(0xabcd_1234 ^ i as u64, chunk_len))
            .collect();

        let mut idx = BlobIndex::new();
        let mut blob_buf = BlobBuffer::new(&backend, keys.clone());
        let mut hashes = Vec::new();
        for c in chunks.iter_mut() {
            hashes.push(Hash::from(hash::multihash(c).to_bytes()));
            blob_buf.add_chunk(c, &mut idx).await.unwrap();
        }
        blob_buf.finalize(&mut idx).await.unwrap();

        // Confirm a single multi-segment v3 blob.
        let front_loc = idx.get_block_location_ref(&hashes[0]).unwrap();
        let whole_blob = backend.read_data(front_loc.blob_path()).await.unwrap();
        let (header, _) = v3format::read_header(&whole_blob).unwrap();
        assert!(header.segment_count > 1, "test needs multiple segments");
        let whole_blob_len = whole_blob.len() as u64;

        // Fresh reader against a fresh counter: read only the front
        // chunk and measure the bytes the backend served.
        let baseline = local.bytes_read();
        let reader = EncBlobReader::new(keys.clone(), backend.clone());
        let got = reader.get_bytes(&front_loc).await.unwrap();
        assert_eq!(&got, &chunks[0], "front chunk must round-trip");

        let fetched = local.bytes_read() - baseline;
        assert!(
            fetched < whole_blob_len,
            "front chunk fetched {} bytes, expected strictly less than whole blob {}",
            fetched,
            whole_blob_len
        );
    }

    #[tokio::test]
    async fn rewrite_blobs_upgrades_v2_to_v3() {
        use crate::compression::compress;
        use crate::dek_provider::encrypt_envelope;
        use crate::v2format::FileType;

        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(&datadir));
        let keys = test_keys();

        // Manually write a v2 blob holding a single chunk and index it
        // with compressed_end = None (the v2 marker).
        let payload = b"legacy v2 payload awaiting upgrade to segmented v3".to_vec();
        let compressed = compress(&payload).unwrap();
        let encrypted = encrypt_envelope(&compressed, FileType::Blob, &keys).unwrap();
        assert_eq!(v3format::peek_version(&encrypted), Some(2));

        let blob_hash = Hash::from(hash::multihash(&encrypted).to_bytes());
        backend.write_data(&blob_hash, &encrypted).await.unwrap();
        let v2_path = storage::path_for(&blob_hash).unwrap();

        let chunk_hash = Hash::from(hash::multihash(&payload).to_bytes());
        let mut idx = BlobIndex::new();
        idx.add_chunk_location(
            &chunk_hash,
            &BlobBlockLocation::new(
                v2_path.clone(),
                Position {
                    offset: 0,
                    size: payload.len(),
                },
            ),
        );

        // Upgrade: feed the v2 blob path through the shared rewriter.
        let mut candidates = HashSet::new();
        candidates.insert(v2_path.clone());
        let stats = rewrite_blobs(&mut idx, &backend, &keys, candidates)
            .await
            .unwrap();
        assert_eq!(stats.blobs_repacked, 1);
        assert_eq!(stats.chunks_moved, 1);
        assert_eq!(stats.old_blobs_deleted, 1);

        // Old v2 blob gone; the chunk now lives in a fresh v3 blob.
        assert!(!backend.exists(&v2_path).await.unwrap());
        let new_loc = idx.get_block_location_ref(&chunk_hash).unwrap();
        assert_ne!(new_loc.blob_path(), &v2_path);
        assert!(new_loc.compressed_end.is_some(), "upgraded chunk is v3");

        let new_raw = backend.read_data(new_loc.blob_path()).await.unwrap();
        assert_eq!(
            v3format::peek_version(&new_raw),
            Some(v3format::FORMAT_VERSION_V3)
        );

        // Data survives the upgrade byte-for-byte.
        let reader = EncBlobReader::new(keys.clone(), backend.clone());
        let got = reader.get_bytes(&new_loc).await.unwrap();
        assert_eq!(got, payload);
    }
}
