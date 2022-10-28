use chrono::NaiveDateTime;
use multihash::{Code, Hasher, MultihashDigest, Sha2_512};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use walkdir::WalkDir;

use crate::format::datetime_format;
use crate::hash::Hash;

use super::blockref::{BlockRef, FileRefLocationIndex};
use super::ChunkMeta;
use super::Chunkerator;
use super::FileRef;
// use super::fileref::FileRef;

const BLOCK_SIZE: usize = 4096;

// const INDEX_FILENAME: &str = "index.dat";
const CURRENT_INDEX_VERSION: &str = "0.2.0";

type FileIndex = HashMap<Hash, FileRef>;
type BlockIndex = HashMap<Hash, BlockRef>;

/// PlainIndex ...
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Eq)]
pub struct PlainIndex {
    // file hash -> FileRef { file: File, paths: HashSet }
    pub(crate) files: HashMap<Hash, FileRef>,
    // plain block hash -> BlockRef
    pub(crate) blocks: HashMap<Hash, BlockRef>,
    pub(crate) version: String,
    #[serde(with = "datetime_format")]
    pub(crate) created_at: NaiveDateTime,
    #[serde(with = "datetime_format")]
    pub(crate) updated_at: NaiveDateTime,
}

impl PlainIndex {
    pub fn new<P: AsRef<Path>>(dir: P) -> Result<Self, Box<dyn std::error::Error>> {
        let (files, blocks) = Self::build_index(dir)?;
        Ok(Self {
            files,
            blocks,
            version: CURRENT_INDEX_VERSION.to_string(),
            created_at: now(),
            updated_at: now(),
        })
    }

    // pub fn read<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
    //     let (files, blocks) = Self::build_index(dir)?;
    //     Ok(Self { files, blocks })
    // }

    fn build_index<P: AsRef<Path>>(
        dir: P,
    ) -> Result<(FileIndex, BlockIndex), Box<dyn std::error::Error>> {
        let mut files = HashMap::<Hash, FileRef>::new();
        let mut blocks = HashMap::<Hash, BlockRef>::new();

        let bludir = dir.as_ref().join(".blu/");
        // TODO: normalize paths by removing `dir` prefix from each elem walked
        for elem in WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()) {
            // skip special .blu dir
            if elem.path().starts_with(&bludir) {
                continue;
            }
            // TODO: allow symlinks?
            if !elem.file_type().is_file() {
                continue;
            }

            // chunking, full file hashing
            let mut chunkmetas: Vec<ChunkMeta> = vec![];
            let mut hasher = Sha2_512::default();
            let chunker = Chunkerator::new(&elem.path(), BLOCK_SIZE)?;
            for chunk in chunker {
                chunkmetas.push(ChunkMeta::new(&chunk));
                hasher.update(&chunk);
            }
            let file_mh = Code::Sha2_512.wrap(hasher.finalize())?;
            let file_hash = Hash::from(file_mh.to_bytes());

            // block index
            let mut offset = 0;
            for cm_ref in chunkmetas.iter() {
                let blockref = blocks
                    .entry(cm_ref.hash.clone())
                    .or_insert_with(BlockRef::new);
                blockref.references.insert(FileRefLocationIndex {
                    size: cm_ref.size,
                    file_hash: file_hash.clone(),
                    offset,
                });
                offset += cm_ref.size;
            }

            // file index
            let fileref = files
                .entry(file_hash)
                .or_insert_with(|| FileRef::new(&chunkmetas));
            fileref.paths.insert(elem.into_path());
        }
        Ok((files, blocks))
    }

    pub fn count_blocks(&self) -> usize {
        self.blocks.len()
    }

    pub fn files_map_ref(&self) -> &HashMap<Hash, FileRef> {
        &self.files
    }

    pub fn blocks_map_ref(&self) -> &HashMap<Hash, BlockRef> {
        &self.blocks
    }

    pub fn get_fileref_ref(&self, file_hash: &Hash) -> Option<&FileRef> {
        self.files.get(file_hash)
    }

    pub fn get_chunk_bytes(&self, blockref: &BlockRef) -> Vec<u8> {
        let disk_index = blockref.references.iter().next().unwrap();
        let fileref = self.get_fileref_ref(&disk_index.file_hash).unwrap();
        let filename = fileref.get_a_path();

        let mut f = std::fs::File::open(filename).unwrap();
        let mut buf: Vec<u8> = vec![0; disk_index.size];
        let _seekptr = f.seek(SeekFrom::Start(disk_index.offset as u64)).unwrap();
        f.read_exact(&mut buf).unwrap();
        buf
    }
}

fn now() -> chrono::NaiveDateTime {
    // returns a NaiveDateTime without milli/nano seconds
    NaiveDateTime::from_timestamp(chrono::Utc::now().timestamp(), 0)
}
