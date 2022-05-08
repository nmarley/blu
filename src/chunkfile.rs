use multihash::{Code, Hasher, MultihashDigest, Sha2_512};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::age::BlackBox;
use crate::dir::Manager;
use crate::hash::{self, Hash};

const DEFAULT_BLOB_INDEX_FILENAME: &str = "blob_index.dat";
const DEFAULT_BLOB_CAPACITY_BYTES: usize = 4_194_304;

// =============================================================================

pub enum CFAddStatus {
    WrittenToDisk,
    AddedToMemory,
    NothingToDo,
}

/// BlobManager writes blob files, re-indexes and re-orgs in case of many
/// chunks (or unused chunks), etc.
#[derive(Debug, Default)]
pub struct BlobManager {
    datadir: PathBuf,
    blob_index: BlobIndex,

    // transient
    data: Vec<u8>,
    blob_capacity: usize,
    offset: usize,
    positions: HashMap<Hash, BlobChunkLocation>,
}

impl BlobManager {
    pub fn new<P: AsRef<Path>>(dir: P, _bbox: &BlackBox) -> Self {
        let datadir = dir.as_ref().to_path_buf();
        let blob_index =
            BlobIndex::deserialize_from_disk(dir.as_ref().join(DEFAULT_BLOB_INDEX_FILENAME))
                .unwrap_or_else(|_| BlobIndex::new());
        Self {
            datadir,
            blob_index,
            data: vec![],
            blob_capacity: DEFAULT_BLOB_CAPACITY_BYTES,
            offset: 0,
            positions: HashMap::new(),
        }
    }

    fn write_blob(&self, data: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let raw_bytes = data;
        let blobfile_hash = Hash::from(hash::multihash(raw_bytes).to_bytes());
        let dir_manager = Manager::new(&self.datadir);
        dir_manager.write_data(&blobfile_hash, &raw_bytes)
    }

    pub fn add_chunk(
        &mut self,
        chunk: &mut [u8],
    ) -> Result<CFAddStatus, Box<dyn std::error::Error>> {
        let chunk_hash = Hash::from(hash::multihash(chunk).to_bytes());
        if self.blob_index.has_chunk(&chunk_hash) {
            return Ok(CFAddStatus::NothingToDo);
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
            self.roll_new_blob()?;
            return Ok(CFAddStatus::WrittenToDisk);
        }

        Ok(CFAddStatus::AddedToMemory)
    }

    fn roll_new_blob(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let path = self.write_blob(&self.data)?;
        for (chunk_hash, location) in self.positions.iter_mut() {
            location.path = path.clone();
            self.blob_index.add_chunk_location(chunk_hash, location);
        }
        self.reset_chunk_stage();
        Ok(())
    }

    // Final blob (in-memory) gets written to disk
    pub fn finalize(&mut self) -> Result<CFAddStatus, Box<dyn std::error::Error>> {
        if self.blob_empty() {
            return Ok(CFAddStatus::NothingToDo);
        }
        self.roll_new_blob()?;
        Ok(CFAddStatus::WrittenToDisk)
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
}

impl std::ops::Drop for BlobManager {
    fn drop(&mut self) {
        let _ = self.finalize().unwrap();
        // TODO: update BI and write to disk
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct Position {
    // where to start reading
    offset: usize,
    // how many bytes to read
    size: usize,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct BlobChunkLocation {
    path: PathBuf,
    position: Position,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Default)]
pub struct BlobIndex {
    // map the encrypted hash to the location of the data on disk
    map: HashMap<Hash, BlobChunkLocation>,
}

impl BlobIndex {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    fn add_chunk_location(&mut self, chunk_hash: &Hash, location: &BlobChunkLocation) {
        self.map.insert(chunk_hash.clone(), location.clone());
    }

    fn has_chunk(&self, chunk_hash: &Hash) -> bool {
        self.map.contains_key(chunk_hash)
    }

    fn deserialize_from_disk<P: AsRef<Path>>(
        index_file_path: P,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let data = std::fs::read(index_file_path.as_ref())?;
        let decoded: BlobIndex = bincode::deserialize(&data)?;
        Ok(decoded)
    }

    fn serialize_to_disk(
        &self,
        index_file_path: impl AsRef<Path>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let encoded = bincode::serialize(&self)?;
        std::fs::write(index_file_path, encoded)?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    // use super::*;

    // helper func used in tests below
    // fn test_chunkfile() -> ChunkFile {
    //     let vec: Vec<Vec<u8>> = vec![
    //         vec![0x0b, 0x0a, 0x00],
    //         vec![0xde, 0xad, 0xbe, 0xef],
    //         vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
    //     ];
    //     let mut cf = ChunkFile::new();
    //     // load w/some data
    //     for v in vec.iter() {
    //         cf.add_chunk(v).unwrap();
    //     }
    //     cf
    // }

    // #[test]
    // fn capacity() {
    //     // NOTE: do not use `test_chunkfile()` here, as we are testing capacity
    //     let vec: Vec<Vec<u8>> = vec![
    //         vec![0x0b, 0x0a, 0x00],
    //         vec![0xde, 0xad, 0xbe, 0xef],
    //         vec![0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e, 0xad],
    //     ];
    //     let mut cf = ChunkFile::with_capacity(3);
    //     // load w/some data
    //     for v in vec.iter() {
    //         cf.add_chunk(v).unwrap();
    //     }
    //     // test get_chunk also
    //     assert_eq!(cf.get_chunk(0).unwrap(), vec![0x0b, 0x0a, 0x00]);
    //     assert_eq!(cf.get_chunk(1).unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
    //     // can't get beyond index
    //     assert!(cf.get_chunk(3).is_err());
    //     // can't add any more, at capacity
    //     assert!(cf.add_chunk(&vec[0]).is_err());
    // }

    // #[test]
    // fn serde() {
    //     let cf = test_chunkfile();
    //     let ser = cf.serialize().unwrap();
    //     let deser = ChunkFile::deserialize(&ser).unwrap();
    //     assert_eq!(cf, deser);
    // }

    // #[test]
    // fn index() {
    //     let cf = test_chunkfile();
    //     let hashes_expected = vec![
    //         ("1340e94518b58bcd5e29a8f6251fbc457c580691c8f9d3e3a17dc404d2e5dc86fa98ac857b8ba9366d6023da1196f89729e760e13fee78c10993c181ecee4211be76", Some(0)),
    //         ("13401284b2d521535196f22175d5f558104220a6ad7680e78b49fa6f20e57ea7b185d71ec1edb137e70eba528dedb141f5d2f8bb53149d262932b27cf41fed96aa7f", Some(1)),
    //         ("13401332e5814224318ddcb3db935b3a7af1f97073b50033be1bc729302028e906f4cb12a652eefe76d7d4f2e8d6bf1671b331f76dc93546e9faa395892fe28d241c", Some(2)),
    //         ("1340cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e", None),
    //     ];
    //     for tuple in hashes_expected.into_iter() {
    //         let (hash, opt) = (Hash::from(tuple.0), tuple.1);
    //         assert_eq!(cf.get_index_for_hash(&hash), opt);
    //     }
    // }

    // #[test]
    // fn blob() {
    //     let mut cf = test_chunkfile();
    //     let fb = cf.flatten();
    //     assert_eq!(
    //         fb.data,
    //         vec![
    //             0x0b, 0x0a, 0x00, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xbe, 0xef, 0x2e,
    //             0xad
    //         ]
    //     );
    //     let positions: HashMap<Hash, Position> = HashMap::from([
    //         (
    //             Hash::from("1340e94518b58bcd5e29a8f6251fbc457c580691c8f9d3e3a17dc404d2e5dc86fa98ac857b8ba9366d6023da1196f89729e760e13fee78c10993c181ecee4211be76"),
    //             Position { offset: 0, size: 3 }
    //         ),
    //         (
    //             Hash::from("13401284b2d521535196f22175d5f558104220a6ad7680e78b49fa6f20e57ea7b185d71ec1edb137e70eba528dedb141f5d2f8bb53149d262932b27cf41fed96aa7f"),
    //             Position { offset: 3, size: 4 },
    //         ),
    //         (
    //             Hash::from("13401332e5814224318ddcb3db935b3a7af1f97073b50033be1bc729302028e906f4cb12a652eefe76d7d4f2e8d6bf1671b331f76dc93546e9faa395892fe28d241c"),
    //             Position { offset: 7, size: 8 },
    //         ),
    //     ]);
    //     assert_eq!(fb.positions, positions);
    // }
}
