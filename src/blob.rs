use core::fmt::Debug;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;

use crate::age::BlackBox;
use crate::block::DEFAULT_CHUNK_SIZE;
use crate::compression::{compress, decompress};
use crate::hash::{self, Hash};
use crate::io::{gen_std_bbserde, BlackBoxSerializable, Position};
use crate::storage::{self, StorageBackend};

/// the default on-disk filename for the blob index
pub const BLOB_INDEX_FILENAME: &str = "blob_index.dat";
// Default chunk size (4096 * 16) * 128 will fit into a blob file by default
// ... around 8MiB
const DEFAULT_BLOB_CAPACITY_BYTES: usize = DEFAULT_CHUNK_SIZE << 7;

// backend::Local
// backend::S3
// backend::DO
// backend::AzureBlob
// backend::GCS

/// BlobBuffer writes blob files, re-indexes and re-orgs in case of many blocks
/// (or unused blocks), etc.
// #[derive(Debug)]
pub struct BlobBuffer<'a> {
    storage_backend: &'a (dyn StorageBackend + 'a),

    // encryption
    bbox: BlackBox,

    // transient
    data: Vec<u8>,
    blob_capacity: usize,
    offset: usize,
    positions: HashMap<Hash, BlobBlockLocation>,
}

// StorageBackend
impl<'a> BlobBuffer<'a> {
    /// Create a new BlobBuffer with the default capacity
    pub fn new(backend: &'a (dyn StorageBackend + 'a), bbox: BlackBox) -> Self {
        Self::with_capacity(backend, bbox, DEFAULT_BLOB_CAPACITY_BYTES)
    }
    /// Create a new BlobBuffer with a specified capacity
    pub fn with_capacity(
        backend: &'a (dyn StorageBackend + 'a),
        bbox: BlackBox,
        capacity: usize,
    ) -> Self {
        Self {
            storage_backend: backend,
            bbox,
            data: vec![],
            blob_capacity: capacity,
            offset: 0,
            positions: HashMap::new(),
        }
    }

    /// Write a block of data to the blob buffer. If the buffer is full, it will be flushed to disk
    /// and a new one started.
    ///
    /// To be used with [`BlobBuffer::finalize`].
    pub fn add_chunk(
        &mut self,
        chunk: &mut [u8],
        idx: &mut BlobIndex,
    ) -> Result<(), Box<dyn std::error::Error>> {
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
            let path = self.roll_new_blob(idx)?;
            debug!("Rolled new blob at {:?}!", path);
            return Ok(());
        }

        debug!("Added chunk to memory!");
        Ok(())
    }

    /// Finalize the blob buffer, writing the last blob to disk and updating the index.
    pub fn finalize(&mut self, idx: &mut BlobIndex) -> Result<(), Box<dyn std::error::Error>> {
        if !self.is_empty() {
            self.roll_new_blob(idx)?;
        }
        Ok(())
    }

    fn write_blob(&self, data: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let raw_bytes = data;
        let blobfile_hash = Hash::from(hash::multihash(raw_bytes).to_bytes());
        self.storage_backend.write_data(&blobfile_hash, raw_bytes)
    }

    // TODO: rename this
    fn roll_new_blob(
        &mut self,
        idx: &mut BlobIndex,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        // compress / encrypt here
        // 1. serialize (done, this is self.data)
        // 2. compress
        // 3. encrypt
        let compressed = compress(&self.data)?;
        let encrypted = self.bbox.encrypt(&compressed)?;

        let path = self.write_blob(&encrypted)?;
        for (chunk_hash, location) in self.positions.iter_mut() {
            location.path = path.clone();
            idx.add_chunk_location(chunk_hash, location);
        }
        self.reset();
        Ok(path)
    }

    // Do not use, for testing only.
    // TODO: Remove this entirely
    fn _eject_blob(&mut self) -> (Vec<u8>, HashMap<Hash, BlobBlockLocation>) {
        let data = std::mem::take(&mut self.data);
        let pos = std::mem::take(&mut self.positions);
        self.reset();
        (data, pos)
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

// NOTE: path should not have .blu or .blu/data in it
// BlobBlockLocation {
//     path: "./.blu/data/9/93c/93c98/93c982e79bcd6d4b32c24af6c4b88c9f9483ab88363a7bd2ae5a1b6da83af1c9163696d946de18ee10510563d3d42e20c52d5b78044a08929ecd2d756d8816d0",
//     position: Position {
//         offset: 65536,
//         size: 65536,
//     },
// }

/// DataCache caches the data from a decrypted blob file.
///
/// This is used to avoid decrypting the same blob file multiple times.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Eq)]
pub struct DataCache {
    // file hash -> bytes
    map: HashMap<Hash, Vec<u8>>,
    limit: usize,
    lru: Vec<Hash>,
}

impl DataCache {
    /// Create a new DataCache with a limit on the number of cached blobs.
    pub fn new(limit: usize) -> Self {
        Self {
            map: HashMap::new(),
            limit,
            lru: vec![],
        }
    }

    /// Get the data from the cache, if it exists.
    pub fn get(&mut self, hash: &Hash) -> Option<&Vec<u8>> {
        self.map.get(hash).map(|data| {
            // update the positions for lru
            let pos = self.lru.iter().position(|x| x == hash).unwrap();
            let hash = self.lru.remove(pos);
            self.lru.push(hash);
            data
        })
    }

    /// Add the data to the cache.
    pub fn add(&mut self, hash: &Hash, data: Vec<u8>) {
        let is_in_lru = self.map.contains_key(hash);
        // add or update data
        self.map.insert(hash.clone(), data);

        if is_in_lru {
            // update the positions for lru
            let pos = self.lru.iter().position(|x| x == hash).unwrap();
            let hash = self.lru.remove(pos);
            self.lru.push(hash);
        } else {
            self.lru.push(hash.clone());
        }

        if self.lru.len() > self.limit {
            let to_remove = self.lru.remove(0);
            self.map.remove(&to_remove);
        }
    }
}

/// EncBlobReader reads encrypted blobs from storage.
pub struct EncBlobReader<'a, 'b> {
    data_cache: DataCache,
    bbox: &'a BlackBox,
    backend: &'b (dyn StorageBackend + 'b),
}
impl<'a, 'b> EncBlobReader<'a, 'b> {
    /// Create a new EncBlobReader.
    pub fn new(bbox: &'a BlackBox, backend: &'b (dyn StorageBackend + 'b)) -> Self {
        Self {
            data_cache: DataCache::new(10),
            bbox,
            backend,
        }
    }

    /// Get the bytes from the blob file at the specified position.
    pub fn get_bytes(
        &mut self,
        location_ref: &BlobBlockLocation,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        // TODO: hash table of paths to blob file hashes, so we don't have to
        // call this shit every time
        let hash = storage::hash_from_path(&location_ref.path)?;

        let full_data_ref = match self.data_cache.get(&hash) {
            Some(data) => {
                info!(
                    "Getting blob file {} from cache",
                    location_ref.path.display()
                );
                data
            }
            None => {
                info!(
                    "Reading blob file from backend: {}",
                    location_ref.path.display()
                );
                let data = &self.backend.read_data(&location_ref.path)?;
                let data = self.bbox.decrypt(data)?;
                let data = decompress(&data)?;
                self.data_cache.add(&hash, data);
                self.data_cache.get(&hash).unwrap()
            }
        };

        let pos = &location_ref.position;
        Ok(full_data_ref[pos.offset..pos.offset + pos.size].to_vec())
    }
}

/// BlobIndex maps the unencrypted chunk hashes to the encrypted blob files and positions within.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Default, Eq)]
pub struct BlobIndex {
    // map the hash of a chunk to the location of the data on disk (within the blob)
    map: HashMap<Hash, BlobBlockLocation>,
}

impl BlobIndex {
    /// Create a new BlobIndex
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Add a chunk location to the index. This should be done after a blob is written to disk.
    ///
    /// Generally the blob buffer will do this in the add_chunk and finalize methods.
    pub fn add_chunk_location(&mut self, chunk_hash: &Hash, location: &BlobBlockLocation) {
        self.map.insert(chunk_hash.clone(), location.clone());
    }

    /// Return whether the block is in the blob index or not.
    ///
    /// This is a good indication of if the block has been encrypted or not.
    pub fn has_chunk(&self, chunk_hash: &Hash) -> bool {
        self.map.contains_key(chunk_hash)
    }

    /// Get the location of the block within the blob.
    pub fn get_block_location_ref(
        &self,
        block_hash: &Hash,
    ) -> Result<BlobBlockLocation, Box<dyn std::error::Error>> {
        let location_ref = self
            .map
            .get(block_hash)
            .ok_or("block hash not found in index")?;
        Ok(location_ref.clone())
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

gen_std_bbserde!(BlobIndex);

#[cfg(test)]
mod test {
    use tempfile::tempdir;

    use super::*;
    use crate::storage::Local;

    const TEST_AGE_SECRET_KEY: &str = include_str!("../test/blu_secrets/blu.key");

    // helper func used in tests below
    fn temp_local_backend() -> Local {
        let datadir = tempdir().unwrap();
        Local::new(datadir)
    }

    fn test_blobbuf<'a>(backend: &'a Local) -> (BlobBuffer<'a>, BlobIndex) {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let mut blob_index = BlobIndex::new();
        let mut blob_buf = BlobBuffer::new(backend, bbox);
        // load w/some data
        for v in vec.iter_mut() {
            blob_buf.add_chunk(v, &mut blob_index).unwrap();
        }
        (blob_buf, blob_index)
    }

    #[test]
    fn new() {
        let backend = temp_local_backend();
        let (mut blob_buf, mut idx) = test_blobbuf(&backend);
        blob_buf.finalize(&mut idx).unwrap();
        assert_eq!(idx.count_blob_files(), 1);
        assert_eq!(idx.count_chunks_indexed(), 3);
    }

    #[test]
    fn capacity() {
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
        let backend = Local::new(&datadir);
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut blob_index = BlobIndex::new();
        let mut blob_buf = BlobBuffer::with_capacity(&backend, bbox, 3);
        // load w/some data
        for v in vec.iter_mut() {
            blob_buf.add_chunk(v, &mut blob_index).unwrap();
        }
        assert_eq!(blob_index.count_blob_files(), 4);
        assert_eq!(blob_index.count_chunks_indexed(), 4);
    }

    #[test]
    fn blob() {
        let backend = temp_local_backend();
        let (mut blob_buf, mut _idx) = test_blobbuf(&backend);
        // TODO: Test the interface, not the implementation
        let (data, positions) = blob_buf._eject_blob();
        assert_eq!(
            data,
            vec![
                0x0b, 0x0a, 0x00, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e,
                0xad
            ]
        );
        let expected_positions: HashMap<Hash, Position> = HashMap::from([
            (
                Hash::from("1340e94518b58bcd5e29a8f6251fbc457c580691c8f9d3e3a17dc404d2e5dc86fa98ac857b8ba9366d6023da1196f89729e760e13fee78c10993c181ecee4211be76"),
                Position { offset: 0, size: 3 }
            ),
            (
                Hash::from("13401284b2d521535196f22175d5f558104220a6ad7680e78b49fa6f20e57ea7b185d71ec1edb137e70eba528dedb141f5d2f8bb53149d262932b27cf41fed96aa7f"),
                Position { offset: 3, size: 4 },
            ),
            (
                Hash::from("13401332e5814224318ddcb3db935b3a7af1f97073b50033be1bc729302028e906f4cb12a652eefe76d7d4f2e8d6bf1671b331f76dc93546e9faa395892fe28d241c"),
                Position { offset: 7, size: 8 },
            ),
        ]);
        let positions: HashMap<Hash, Position> = positions
            .into_iter()
            .map(|(k, v)| (k, v.position))
            .collect();
        assert_eq!(positions, expected_positions);
    }

    #[test]
    fn data_cache() {
        let a_hash = Hash::from("1340aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b_hash = Hash::from("1340bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let c_hash = Hash::from("1340cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc");

        let mut dc = super::DataCache::new(2);
        dc.add(&a_hash, vec![0x0b, 0x0a, 0x00]);
        dc.add(&b_hash, vec![0x00, 0x0a, 0x0b]);

        assert_eq!(dc.get(&a_hash), Some(&vec![0x0b, 0x0a, 0x00]));
        assert_eq!(dc.get(&b_hash), Some(&vec![0x00, 0x0a, 0x0b]));

        dc.add(&a_hash, vec![0x00, 0x0a, 0x0b]);
        dc.add(&c_hash, vec![0x0c, 0x0c, 0x0c]);

        assert_eq!(dc.get(&b_hash), None);
        assert_eq!(dc.get(&c_hash), Some(&vec![0x0c, 0x0c, 0x0c]));
        assert_eq!(dc.get(&a_hash), Some(&vec![0x00, 0x0a, 0x0b]));
        dc.add(&b_hash, vec![0x00, 0x0a, 0x0b]);

        // added a, then c, then b... but most recently used a, before adding b,
        // so C should now be removed
        assert_eq!(dc.get(&c_hash), None);

        dbg!(&dc);
    }
}
