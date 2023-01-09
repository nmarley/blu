use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::age::BlackBox;
use crate::compression::{compress, decompress};
use crate::dir::Manager;
use crate::hash::{self, Hash};
use crate::io::{gen_std_bbserde, BlackBoxSerializable};

/// the default on-disk filename for the blob index
pub const BLOB_INDEX_FILENAME: &str = "blob_index.dat";
const DEFAULT_BLOB_CAPACITY_BYTES: usize = 4_194_304;

/// BlobBuffer writes blob files, re-indexes and re-orgs in case of many blocks (or unused blocks),
/// etc.
#[derive(Debug)]
pub struct BlobBuffer {
    // TODO: Remove this in favor of a trait / implementation?
    //    e.g. the writer could be a FileBlobWriter, or a S3BlobWriter (CloudBlobWriter?), etc.
    datadir: PathBuf,

    // encryption
    bbox: BlackBox,

    // transient
    data: Vec<u8>,
    blob_capacity: usize,
    offset: usize,
    positions: HashMap<Hash, BlobChunkLocation>,
}

impl BlobBuffer {
    /// Create a new BlobBuffer with the default capacity
    pub fn new<P: AsRef<Path>>(dir: P, bbox: BlackBox) -> Self {
        Self::with_capacity(dir, bbox, DEFAULT_BLOB_CAPACITY_BYTES)
    }
    /// Create a new BlobBuffer with a specified capacity
    pub fn with_capacity<P: AsRef<Path>>(dir: P, bbox: BlackBox, capacity: usize) -> Self {
        let datadir = dir.as_ref().to_path_buf();
        Self {
            datadir,
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
            BlobChunkLocation {
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
            println!("Rolled new blob at {:?}!", path);
            return Ok(());
        }

        println!("Added chunk to memory!");
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
        let dir_manager = Manager::new(&self.datadir);
        // TODO: not a fan of this design ... :/
        // maybe move dir_manager to BlobBuffer?
        dir_manager.write_data(&blobfile_hash, raw_bytes)
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
    fn _eject_blob(&mut self) -> (Vec<u8>, HashMap<Hash, BlobChunkLocation>) {
        let data = self.data.clone();
        let pos = self.positions.clone();
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
    /// Read the data from the blob file at the specified position
    pub fn get_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut f = std::fs::File::open(&self.path)?;
        let mut buf: Vec<u8> = vec![0; self.position.size];
        let _seekptr = f.seek(SeekFrom::Start(self.position.offset as u64))?;
        f.read_exact(&mut buf)?;
        Ok(buf)
    }
}

/// BlobIndex maps the plain hashes to the encrypted blob files and positions within.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Default, Eq)]
pub struct BlobIndex {
    // map the hash to the location of the data on disk
    map: HashMap<Hash, BlobChunkLocation>,
    // Do not re-serialize to disk if the blob index wasn't modified.
    #[serde(skip)]
    modified: bool,
}

impl BlobIndex {
    /// Create a new BlobIndex
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            modified: false,
        }
    }

    /// Add a chunk location to the index. This should be done after a blob is written to disk.
    ///
    /// Generally the blob buffer will do this in the add_chunk and finalize methods.
    pub fn add_chunk_location(&mut self, chunk_hash: &Hash, location: &BlobChunkLocation) {
        self.map.insert(chunk_hash.clone(), location.clone());
        self.modified = true;
    }

    /// Return whether the block is in the blob index or not.
    ///
    /// This is a good indication of if the block has been encrypted or not.
    pub fn has_chunk(&self, chunk_hash: &Hash) -> bool {
        self.map.contains_key(chunk_hash)
    }

    // #[allow(dead_code)]
    // /// Get a block of data from the given blob index.
    // ///
    // /// WARNING: This was implemented before encryption, so does not work as-is right now. Might be
    // /// useful for restores or reconciliation (for dangling blob chunks / index entries).
    // pub fn get_chunk_bytes(
    //     &self,
    //     chunk_hash: &Hash,
    // ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    //     let location_ref = self
    //         .map
    //         .get(chunk_hash)
    //         .ok_or("chunk hash not found in index")?;
    //     location_ref.get_bytes()
    // }

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
    use super::*;
    use tempfile::tempdir;

    const TEST_AGE_SECRET_KEY: &str = include_str!("../test/blu_secrets/blu.key");

    // helper func used in tests below
    fn test_blobbuf() -> (BlobBuffer, BlobIndex) {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let datadir = tempdir().unwrap();
        let mut blob_index = BlobIndex::new();
        let mut blob_buf = BlobBuffer::new(&datadir, bbox);
        // load w/some data
        for v in vec.iter_mut() {
            blob_buf.add_chunk(v, &mut blob_index).unwrap();
        }
        (blob_buf, blob_index)
    }

    #[test]
    fn new() {
        let (mut blob_buf, mut idx) = test_blobbuf();
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
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut blob_index = BlobIndex::new();
        let mut blob_buf = BlobBuffer::with_capacity(&datadir, bbox, 3);
        // load w/some data
        for v in vec.iter_mut() {
            blob_buf.add_chunk(v, &mut blob_index).unwrap();
        }
        assert_eq!(blob_index.count_blob_files(), 4);
        assert_eq!(blob_index.count_chunks_indexed(), 4);
    }

    #[test]
    fn blob() {
        let (mut blob_buf, mut _idx) = test_blobbuf();
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
}
