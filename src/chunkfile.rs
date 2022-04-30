use multihash::{Code, Hasher, MultihashDigest, Sha2_512};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};

use crate::dir::Manager;
use crate::hash::{self, Hash};

const DEFAULT_CHUNKFILE_CAPACITY: usize = 1024;
const DEFAULT_CFI_NAME: &str = "cfi.dat";

// =============================================================================

pub enum CFAddStatus {
    WrittenToDisk(PathBuf),
    AddedToMemory,
    NothingToDo,
}

/// ChunkFileManager writes chunkfiles, re-indexes and re-orgs in case of many
/// unused chunks, etc.
#[derive(Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct ChunkFileManager {
    datadir: PathBuf,
    chunkfile_index: ChunkFileIndex,
    active_chunkfile: ChunkFile,
    // ChunkFileIndex
    // encrypted hash -> location of the data on disk
    // map: HashMap<Hash, EncChunkLocation>,

    // EncChunkLocation
    //     path: PathBuf,
    //     index: usize,

    // ChunkFile
    //     // this is a vector of encryted data chunks -- NOT HASHES
    //     chunks: Vec<Vec<u8>>,
    //     capacity: usize,
    //     // this is the hash / index into the chunkfile, e.g. the hash of the
    //     // encrypted data chunk can be found in `chunks` at index usize>
    //     positions: HashMap<Hash, usize>,
}

impl ChunkFileManager {
    pub fn new<P: AsRef<Path>>(dir: P) -> Self {
        let datadir = dir.as_ref().to_path_buf();
        let cfi = ChunkFileIndex::deserialize_from_disk(dir.as_ref().join(DEFAULT_CFI_NAME))
            .unwrap_or_else(|_| ChunkFileIndex::new());
        Self {
            datadir,
            chunkfile_index: cfi,
            active_chunkfile: ChunkFile::new(),
        }
    }

    fn write_chunkfile(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let raw_bytes = &self.active_chunkfile.serialize()?;
        let dir_manager = Manager::new(&self.datadir);
        let chunkfile_hash = &self.active_chunkfile.hash();
        dir_manager.write_encrypted(chunkfile_hash, raw_bytes)
    }

    // TODO: not sure on retval here ... should it be sth that returns EITHER a
    // CF::Written or a CF::Memory, or sth like that?
    pub fn add_chunk(&mut self, chunk: &[u8]) -> Result<CFAddStatus, Box<dyn std::error::Error>> {
        if self.active_chunkfile.is_full() {
            // TODO: caller should add Paths from here to Index
            let path = self.write_chunkfile()?;
            self.active_chunkfile = ChunkFile::new();
            return Ok(CFAddStatus::WrittenToDisk(path));
        }
        self.active_chunkfile.add_chunk(chunk)?;
        Ok(CFAddStatus::AddedToMemory)
    }

    // Final chunkfile (in-memory) gets written to disk
    pub fn finalize(&mut self) -> Result<CFAddStatus, Box<dyn std::error::Error>> {
        if self.active_chunkfile.is_empty() {
            return Ok(CFAddStatus::NothingToDo);
        }
        let path = self.write_chunkfile()?;
        Ok(CFAddStatus::WrittenToDisk(path))
    }
}

impl std::ops::Drop for ChunkFileManager {
    fn drop(&mut self) {
        let _ = self.finalize().unwrap();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ChunkFile {
    // this is a vector of encryted data chunks -- NOT HASHES
    chunks: Vec<Vec<u8>>,
    capacity: usize,

    // this is the hash / index into the chunkfile, e.g. the hash of the
    // encrypted data chunk can be found in `chunks` at index usize>
    positions: HashMap<Hash, usize>,
}

impl ChunkFile {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CHUNKFILE_CAPACITY)
    }
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            chunks: Vec::with_capacity(capacity),
            positions: HashMap::new(),
        }
    }

    pub fn add_chunk(&mut self, chunk: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_full() {
            return Err("capacity has been reached".into());
        }

        let index = self.count();
        let hash = Hash::from(hash::multihash(chunk).to_bytes());
        self.positions.insert(hash, index);

        self.chunks.push(chunk.to_vec());
        Ok(())
    }

    pub fn count(&self) -> usize {
        self.chunks.len()
    }

    pub fn get_chunk(&self, index: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        if index >= self.capacity {
            return Err(
                format!("index {} greater than capacity of {}", index, self.capacity).into(),
            );
        }
        Ok(self.chunks[index].to_vec())
    }

    pub fn get_index_for_hash(&self, hash: &Hash) -> Option<usize> {
        self.positions.get(hash).copied()
    }

    pub fn serialize(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let encoded: Vec<u8> = bincode::serialize(self)?;
        // let encoded: Vec<u8> = serde_cbor::to_vec(self)?;
        Ok(encoded)
    }

    pub fn deserialize(data: &[u8]) -> Result<ChunkFile, Box<dyn std::error::Error>> {
        // let decoded: ChunkFile = serde_cbor::from_slice(data)?;
        let decoded: ChunkFile = bincode::deserialize(data)?;
        Ok(decoded)
    }

    // hash all the chunks and get the result
    pub fn hash(&self) -> Hash {
        let mut h = Sha2_512::default();
        for chunk_bytes in self.chunks.iter() {
            h.update(chunk_bytes)
        }
        let digest = h.finalize();
        let multihash = Code::Sha2_512.wrap(digest).unwrap();
        Hash::from(multihash.to_bytes())
    }

    pub fn is_full(&self) -> bool {
        self.count() >= self.capacity
    }

    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    pub fn flatten(&mut self) -> FlatBlob {
        let mut offset: usize = 0;
        let mut data: Vec<u8> = vec![];
        let mut positions: HashMap<Hash, Position> = HashMap::new();
        for chunk in self.chunks.iter_mut() {
            let hash = Hash::from(hash::multihash(chunk).to_bytes());
            let size = chunk.len();
            positions.insert(hash, Position { offset, size });
            offset += size;
            data.append(chunk);
        }
        FlatBlob { data, positions }
    }
}

/// FlatBlob is a "flattened" version of the ChunkFile above. The Vec<Vec<u8>>
/// is flattened into a single Vec<u8>, and the positions is converted from a
/// usize index, into an offset and number of bytes.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct FlatBlob {
    data: Vec<u8>,
    positions: HashMap<Hash, Position>,
}
// impl FlatBlob {
// }

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
struct Position {
    // where to start reading
    offset: usize,
    // how many bytes to read
    size: usize,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct EncChunkLocation {
    path: PathBuf,
    index: usize,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Default)]
pub struct ChunkFileIndex {
    // map the encrypted hash to the location of the data on disk
    map: HashMap<Hash, EncChunkLocation>,
}

impl ChunkFileIndex {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    fn add_chunk_location(&mut self, chunk_hash: &Hash, location: &EncChunkLocation) {
        self.map.insert(chunk_hash.clone(), location.clone());
    }

    fn deserialize_from_disk<P: AsRef<Path>>(
        datadir: P,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let data = std::fs::read(datadir.as_ref())?;
        let decoded: ChunkFileIndex = bincode::deserialize(&data)?;
        Ok(decoded)
    }

    // returns the encrypted from disk, decrypt it yourself
    //
    // TODO: seems REALLY weird to just open a new ChunkFile on disk every time
    // to read a single block ... should we maintain a map of open files for
    // reading the chunks? e.g. once this particular location is opened, we
    // don't close it, keep it open at least for X most recently accessed
    // files?
    fn get_enc_block(&self, hash: &Hash) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let enc_location = self.map.get(hash).ok_or("location not found")?;

        let mut f = std::fs::File::open(&enc_location.path)?;
        let mut chunkdata = Vec::new();
        let _bytes_read = f.read(&mut chunkdata)?;
        let chunkfile = ChunkFile::deserialize(&chunkdata)?;

        chunkfile.get_chunk(enc_location.index)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    // helper func used in tests below
    fn test_chunkfile() -> ChunkFile {
        let vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let mut cf = ChunkFile::new();
        // load w/some data
        for v in vec.iter() {
            cf.add_chunk(v).unwrap();
        }
        cf
    }

    #[test]
    fn capacity() {
        // NOTE: do not use `test_chunkfile()` here, as we are testing capacity
        let vec: Vec<Vec<u8>> = vec![
            vec![0x0b, 0x0a, 0x00],
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
        ];
        let mut cf = ChunkFile::with_capacity(3);
        // load w/some data
        for v in vec.iter() {
            cf.add_chunk(v).unwrap();
        }
        // test get_chunk also
        assert_eq!(cf.get_chunk(0).unwrap(), vec![0x0b, 0x0a, 0x00]);
        assert_eq!(cf.get_chunk(1).unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);

        // can't get beyond index
        assert!(cf.get_chunk(3).is_err());
        // can't add any more, at capacity
        assert!(cf.add_chunk(&vec[0]).is_err());
    }

    #[test]
    fn serde() {
        let cf = test_chunkfile();
        let ser = cf.serialize().unwrap();
        let deser = ChunkFile::deserialize(&ser).unwrap();
        assert_eq!(cf, deser);
    }

    #[test]
    fn index() {
        let cf = test_chunkfile();
        let hashes_expected = vec![
            ("1340e94518b58bcd5e29a8f6251fbc457c580691c8f9d3e3a17dc404d2e5dc86fa98ac857b8ba9366d6023da1196f89729e760e13fee78c10993c181ecee4211be76", Some(0)),
            ("13401284b2d521535196f22175d5f558104220a6ad7680e78b49fa6f20e57ea7b185d71ec1edb137e70eba528dedb141f5d2f8bb53149d262932b27cf41fed96aa7f", Some(1)),
            ("13401332e5814224318ddcb3db935b3a7af1f97073b50033be1bc729302028e906f4cb12a652eefe76d7d4f2e8d6bf1671b331f76dc93546e9faa395892fe28d241c", Some(2)),
            ("1340cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e", None),
        ];
        for tuple in hashes_expected.into_iter() {
            let (hash, opt) = (Hash::from(tuple.0), tuple.1);
            assert_eq!(cf.get_index_for_hash(&hash), opt);
        }
    }

    #[test]
    fn flatblob() {
        let mut cf = test_chunkfile();
        let fb = cf.flatten();
        assert_eq!(
            fb.data,
            vec![
                0x0b, 0x0a, 0x00, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e,
                0xad
            ]
        );
        let positions: HashMap<Hash, Position> = HashMap::from([
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
        assert_eq!(fb.positions, positions);
    }
}
