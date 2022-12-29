use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::age::BlackBox;
use crate::dir::Manager;
use crate::hash::{self, Hash};

const DEFAULT_BLOB_INDEX_FILENAME: &str = "blob_index.dat";
const DEFAULT_BLOB_CAPACITY_BYTES: usize = 4_194_304;

/// BlobManager writes blob files, re-indexes and re-orgs in case of many
/// chunks (or unused chunks), etc.
#[derive(Debug, Default)]
pub struct BlobManager {
    datadir: PathBuf,
    blob_index: BlobIndex,

    // encryption
    bbox: Option<BlackBox>,

    // transient
    data: Vec<u8>,
    blob_capacity: usize,
    offset: usize,
    positions: HashMap<Hash, BlobChunkLocation>,
}

impl BlobManager {
    pub fn new<P: AsRef<Path>>(dir: P, bbox: Option<BlackBox>) -> Self {
        Self::with_capacity(dir, bbox, DEFAULT_BLOB_CAPACITY_BYTES)
    }
    pub fn with_capacity<P: AsRef<Path>>(dir: P, bbox: Option<BlackBox>, capacity: usize) -> Self {
        let datadir = dir.as_ref().to_path_buf();
        let blob_index =
            BlobIndex::deserialize_from_disk(dir.as_ref().join(DEFAULT_BLOB_INDEX_FILENAME))
                .unwrap_or_else(|_| BlobIndex::new());
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
        // TODO: compress / encrypt here
        let data = match &self.bbox {
            Some(bbox) => bbox.encrypt(&self.data)?,
            None => self.data.clone(),
        };
        // let encrypted = self.bbox.encrypt(&self.data)?;

        let path = self.write_blob(&data)?;
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
        self.blob_index
            .serialize_to_disk(self.datadir.as_path().join(DEFAULT_BLOB_INDEX_FILENAME))?;
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

/// BlobIndex maps the plain hash to the blob and position within. This is
/// managed by the BlobManager.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Default, Eq)]
pub struct BlobIndex {
    // map the hash to the location of the data on disk
    map: HashMap<Hash, BlobChunkLocation>,
    // Do not re-serialize to disk if the blob index wasn't modified.
    #[serde(skip)]
    modified: bool,
}

impl BlobIndex {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            modified: false,
        }
    }

    fn add_chunk_location(&mut self, chunk_hash: &Hash, location: &BlobChunkLocation) {
        self.map.insert(chunk_hash.clone(), location.clone());
        self.modified = true;
    }

    fn has_chunk(&self, chunk_hash: &Hash) -> bool {
        self.map.contains_key(chunk_hash)
    }

    // #[allow(dead_code)]
    fn get_chunk_bytes(&self, chunk_hash: &Hash) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let location_ref = self
            .map
            .get(chunk_hash)
            .ok_or("chunk hash not found in index")?;
        location_ref.get_bytes()
    }

    fn deserialize_from_disk<P: AsRef<Path>>(
        index_file_path: P,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let data = std::fs::read(index_file_path.as_ref())?;
        let decoded: BlobIndex = bincode::deserialize(&data)?;
        Ok(decoded)
    }

    fn serialize_to_disk<P: AsRef<Path>>(
        &self,
        index_file_path: P,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.modified {
            let encoded = bincode::serialize(&self)?;
            std::fs::write(index_file_path, encoded)?;
        }
        Ok(())
    }

    // TODO: this is inefficient to call multiple times in a row
    fn count_blob_files(&self) -> usize {
        let mut set = HashSet::<PathBuf>::new();
        for loc in self.map.values() {
            set.insert(loc.path.clone());
        }
        set.len()
    }

    fn count_chunks_indexed(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use tempfile::tempdir;

    // helper func used in tests below
    fn test_blobmgr() -> BlobManager {
        let mut vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let datadir = tempdir().unwrap();
        let mut blob_mgr = BlobManager::new(&datadir, None);
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
        let mut blob_mgr = BlobManager::with_capacity(&datadir, None, 3);
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
