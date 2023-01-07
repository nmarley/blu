use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::age::BlackBox;
use crate::compression::{compress, decompress};
use crate::dir::Manager;
use crate::hash::{self, Hash};
use crate::io::BlackBoxSerializable;

// const DEFAULT_BLOB_INDEX_FILENAME: &str = "blob_index.dat";
pub const BLOB_INDEX_FILENAME: &str = "blob_index.dat";
const DEFAULT_BLOB_CAPACITY_BYTES: usize = 4_194_304;

/// BlobManager writes blob files, re-indexes and re-orgs in case of many
/// chunks (or unused chunks), etc.
#[derive(Debug)]
pub struct BlobManager {
    datadir: PathBuf,
    blob_index: BlobIndex,

    // encryption
    bbox: BlackBox,

    // transient
    data: Vec<u8>,
    blob_capacity: usize,
    offset: usize,
    positions: HashMap<Hash, BlobChunkLocation>,
}

impl BlobManager {
    pub fn new<P: AsRef<Path>>(dir: P, bbox: BlackBox, blob_index: BlobIndex) -> Self {
        Self::with_capacity(dir, bbox, blob_index, DEFAULT_BLOB_CAPACITY_BYTES)
    }
    pub fn with_capacity<P: AsRef<Path>>(
        dir: P,
        bbox: BlackBox,
        blob_index: BlobIndex,
        capacity: usize,
    ) -> Self {
        let datadir = dir.as_ref().to_path_buf();
        Self {
            datadir,
            blob_index,
            bbox,
            data: vec![],
            blob_capacity: capacity,
            offset: 0,
            positions: HashMap::new(),
        }
    }

    fn write_blob(&self, data: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let raw_bytes = data;
        let blobfile_hash = Hash::from(hash::multihash(raw_bytes).to_bytes());
        let dir_manager = Manager::new(&self.datadir);
        // TODO: not a fan of this design ... :/
        dir_manager.write_data(&blobfile_hash, raw_bytes)
    }

    pub fn add_chunk(&mut self, chunk: &mut [u8]) -> Result<(), Box<dyn std::error::Error>> {
        let chunk_hash = Hash::from(hash::multihash(chunk).to_bytes());
        if self.blob_index.has_chunk(&chunk_hash) {
            println!(
                "Already found chunk {:?} in blob index, nothing to add",
                &chunk_hash
            );
            return Ok(());
        }

        let mut chunk_copy = chunk.to_vec();
        self.data.append(&mut chunk_copy);
        let size = chunk.len();
        // remap the path after writing and then add to the blob_index
        self.positions.insert(
            chunk_hash,
            BlobChunkLocation {
                path: "".into(),
                position: Position {
                    offset: self.offset,
                    size,
                },
            },
        );
        self.offset += size;

        if self.blob_full() {
            let path = self.roll_new_blob()?;
            println!("Rolled new blob at {:?}!", path);
            return Ok(());
        }

        println!("Added chunk to memory!");
        Ok(())
    }

    fn roll_new_blob(&mut self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        // compress / encrypt here
        // 1. serialize (done, this is self.data)
        // 2. compress
        // 3. encrypt
        let compressed = compress(&self.data)?;
        let encrypted = self.bbox.encrypt(&compressed)?;

        let path = self.write_blob(&encrypted)?;
        for (chunk_hash, location) in self.positions.iter_mut() {
            location.path = path.clone();
            self.blob_index.add_chunk_location(chunk_hash, location);
        }
        self.reset_chunk_stage();
        Ok(path)
    }

    /// Do not use, for testing only.
    fn _eject_blob(&mut self) -> (Vec<u8>, HashMap<Hash, BlobChunkLocation>) {
        let data = self.data.clone();
        let pos = self.positions.clone();
        self.reset_chunk_stage();
        (data, pos)
    }

    // Final blob (in-memory) gets written to disk
    pub fn finalize(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if !self.blob_empty() {
            self.roll_new_blob()?;
        }
        if self.blob_index.modified {
            let blob_index_path = self.datadir.as_path().join(BLOB_INDEX_FILENAME);
            let mut buf = Vec::new();
            self.blob_index.write(&mut buf, &self.bbox)?;
            std::fs::write(blob_index_path, buf)?;
        }
        Ok(())
    }

    fn blob_full(&self) -> bool {
        self.data.len() >= self.blob_capacity
    }
    fn blob_empty(&self) -> bool {
        self.data.is_empty()
    }
    fn reset_chunk_stage(&mut self) {
        self.data = vec![];
        self.offset = 0;
        self.positions = HashMap::new();
    }

    pub fn count_blob_files(&self) -> usize {
        self.blob_index.count_blob_files()
    }

    pub fn count_chunks_indexed(&self) -> usize {
        self.blob_index.count_chunks_indexed()
    }
}

impl std::ops::Drop for BlobManager {
    fn drop(&mut self) {
        self.finalize().unwrap();
    }
}

/// Position is the offset and size of a chunk of data within a bigger blob of
/// data.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Eq)]
pub struct Position {
    // where to start reading
    offset: usize,
    // how many bytes to read
    size: usize,
}

/// BlobChunkLocation is a path to a blob file and a position (offset/size)
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Eq)]
pub struct BlobChunkLocation {
    path: PathBuf,
    position: Position,
}

impl BlobChunkLocation {
    pub fn get_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut f = std::fs::File::open(&self.path)?;
        let mut buf: Vec<u8> = vec![0; self.position.size];
        let _seekptr = f.seek(SeekFrom::Start(self.position.offset as u64))?;
        f.read_exact(&mut buf)?;
        Ok(buf)
    }
}

/// BlobIndex maps the plain hash to the encrypted blobfile and position within.
/// This is managed by the BlobManager.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Default, Eq)]
pub struct BlobIndex {
    // map the hash to the location of the data on disk
    map: HashMap<Hash, BlobChunkLocation>,
    // Do not re-serialize to disk if the blob index wasn't modified.
    #[serde(skip)]
    modified: bool,
}

impl BlobIndex {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            modified: false,
        }
    }

    pub fn add_chunk_location(&mut self, chunk_hash: &Hash, location: &BlobChunkLocation) {
        self.map.insert(chunk_hash.clone(), location.clone());
        self.modified = true;
    }

    pub fn has_chunk(&self, chunk_hash: &Hash) -> bool {
        self.map.contains_key(chunk_hash)
    }

    #[allow(dead_code)]
    pub fn get_chunk_bytes(
        &self,
        chunk_hash: &Hash,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let location_ref = self
            .map
            .get(chunk_hash)
            .ok_or("chunk hash not found in index")?;
        location_ref.get_bytes()
    }

    pub fn count_blob_files(&self) -> usize {
        self.map
            .values()
            .map(|loc| &loc.path)
            .collect::<HashSet<&PathBuf>>()
            .len()
    }

    pub fn count_chunks_indexed(&self) -> usize {
        self.map.len()
    }
}

impl BlackBoxSerializable for BlobIndex {
    fn write<W: io::Write>(
        &self,
        mut stream: W,
        bbox: &BlackBox,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let serialized = self.serialize_bytes()?;
        let compressed = compress(&serialized)?;
        let encrypted = bbox.encrypt(&compressed)?;
        let _ = stream.write_all(&encrypted);
        Ok(())
    }

    fn deserialize_bytes(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        // let decoded: Index = serde_cbor::from_slice(data)?;
        let decoded: Self = bincode::deserialize(data)?;
        Ok(decoded)
    }

    fn serialize_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let encoded: Vec<u8> = bincode::serialize(&self)?;
        // let encoded: Vec<u8> = serde_cbor::to_vec(&self)?;
        Ok(encoded)
    }

    // read / write serialization methods integrate BlackBox for automagic
    // also compress and decompress
    fn read<R: io::Read>(
        mut stream: R,
        bbox: &BlackBox,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut encrypted = Vec::new();
        let _ = stream.read_to_end(&mut encrypted)?;
        let compressed = bbox.decrypt(&encrypted)?;
        let serialized = decompress(&compressed)?;
        Self::deserialize_bytes(&serialized)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use tempfile::tempdir;

    const TEST_AGE_SECRET_KEY: &str = include_str!("../test/blu_secrets/blu.key");

    // helper func used in tests below
    fn test_blobmgr() -> BlobManager {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let datadir = tempdir().unwrap();
        let blob_index = BlobIndex::new();
        let mut blob_mgr = BlobManager::new(&datadir, bbox, blob_index);
        // load w/some data
        for v in vec.iter_mut() {
            blob_mgr.add_chunk(v).unwrap();
        }
        blob_mgr
    }

    #[test]
    fn new() {
        let mut blob_mgr = test_blobmgr();
        blob_mgr.finalize().unwrap();
        assert_eq!(blob_mgr.count_blob_files(), 1);
        assert_eq!(blob_mgr.count_chunks_indexed(), 3);
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
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let blob_index = BlobIndex::new();
        let mut blob_mgr = BlobManager::with_capacity(&datadir, bbox, blob_index, 3);
        // load w/some data
        for v in vec.iter_mut() {
            blob_mgr.add_chunk(v).unwrap();
        }
        assert_eq!(blob_mgr.count_blob_files(), 4);
        assert_eq!(blob_mgr.count_chunks_indexed(), 4);
    }

    #[test]
    fn blob() {
        let mut blob_mgr = test_blobmgr();
        let (data, positions) = blob_mgr._eject_blob();
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
}
