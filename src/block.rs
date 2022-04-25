use multihash::{Code, Hasher, Multihash, MultihashDigest, Sha2_512};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::hash::{self, Hash};

const BLOCK_SIZE: usize = 4096;

/// PlainFileIndex keeps a map of file data hash to a FileRef
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct PlainFileIndex {
    // file hash -> FileRef { file: File, paths: HashSet }
    map: HashMap<Hash, FileRef>,
}

impl PlainFileIndex {
    pub fn new<P: AsRef<Path>>(dir: P) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            map: Self::build_file_index(dir)?,
        })
    }

    fn build_file_index<P: AsRef<Path>>(
        dir: P,
    ) -> Result<HashMap<Hash, FileRef>, Box<dyn std::error::Error>> {
        let mut map: HashMap<Hash, FileRef> = HashMap::new();
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

            let mut chunkmetas: Vec<ChunkMeta> = vec![];
            let mut hasher = Sha2_512::default();
            let chunker = Chunkerator::new(&elem.path(), BLOCK_SIZE)?;
            for chunk in chunker {
                chunkmetas.push(ChunkMeta::new(&chunk));
                hasher.update(&chunk);
            }
            let file_mh = Code::Sha2_512.wrap(hasher.finalize())?;
            let file_hash = Hash::from(file_mh.to_bytes());

            let fileref = map
                .entry(file_hash)
                .or_insert_with(|| FileRef::new(&chunkmetas));
            fileref.paths.insert(elem.into_path());
        }
        Ok(map)
    }

    pub fn map_ref(&self) -> &HashMap<Hash, FileRef> {
        &self.map
    }
}

/// PlainBlockIndex contains a map of "plain" (non-encrypted) chunk hash to BlockRef.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct PlainBlockIndex {
    // plain block hash -> BlockRef
    // TODO: inline BlockRef data and remove intermediate type?
    map: HashMap<Hash, BlockRef>,
}

impl PlainBlockIndex {
    pub fn new(file_index: &PlainFileIndex) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            map: Self::build_block_index(&file_index.map),
        })
    }

    fn build_block_index(file_index: &HashMap<Hash, FileRef>) -> HashMap<Hash, BlockRef> {
        let mut block_index = HashMap::<Hash, BlockRef>::new();
        for (file_hash, fr) in file_index.iter() {
            for cm in fr.chunkmetas.iter() {
                let blockref = block_index
                    .entry(cm.hash.clone())
                    .or_insert_with(BlockRef::new);
                blockref.referencing_file_hashes.insert(file_hash.clone());
            }
        }
        block_index
    }

    pub fn count_blocks(&self) -> usize {
        self.map.len()
    }
}

// blockref -> option<enc hash>
//          -> set of referencing file hashes
/// BlockRef has a collection of file hashes which reference a particular block.
#[derive(Default, Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct BlockRef {
    // TBD: this has to be integrated w/the ChunkFileIndex and encryptor
    //
    // encrypted_hash: Option<Hash>,

    // hashes of the files which reference this block
    referencing_file_hashes: HashSet<Hash>,
}

impl BlockRef {
    fn new() -> Self {
        Self {
            // encrypted_hash: None,
            referencing_file_hashes: HashSet::new(),
        }
    }
}

/// FileRef is a container encapsulating a Vec<ChunkMeta> (collection of hashes
/// of chunks read from a fs::File) and filesystem references to it (filenames)
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct FileRef {
    chunkmetas: Vec<ChunkMeta>,
    paths: HashSet<PathBuf>,
    // TODO: filetype, tags, notes?
}

impl FileRef {
    pub fn new(f: &[ChunkMeta]) -> Self {
        Self {
            chunkmetas: f.to_vec(),
            paths: HashSet::new(),
        }
    }

    // open a file and iterate over the chunks
    pub fn iter(&self) -> Result<FileRefIterator, Box<dyn std::error::Error>> {
        if self.paths.is_empty() {
            return Err("no path from which to read bytes".into());
        }
        let path = self.paths.iter().next().unwrap();
        Ok(FileRefIterator::new(&self.chunkmetas, path.to_path_buf()))
    }
}

/// FileRefIterator basically does what Chunkerator does... might be able to
/// replace or remove this entirely.
#[derive(Debug, PartialEq, Clone)]
pub struct FileRefIterator {
    chunkmetas: Vec<ChunkMeta>,
    path: PathBuf,
    iterpos: usize,
    offset: u64,
}

impl FileRefIterator {
    pub fn new(f: &[ChunkMeta], path: PathBuf) -> Self {
        Self {
            chunkmetas: f.to_vec(),
            path,
            iterpos: 0,
            offset: 0,
        }
    }
}

impl std::iter::Iterator for FileRefIterator {
    type Item = Vec<u8>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.iterpos >= self.chunkmetas.len() {
            return None;
        }
        let block = &self.chunkmetas[self.iterpos];

        // read block.size bytes
        let mut f = std::fs::File::open(&self.path).unwrap();
        let mut buf: Vec<u8> = vec![0u8; block.size];
        let _seeko = f.seek(SeekFrom::Start(self.offset)).unwrap();
        f.read_exact(&mut buf).unwrap();

        self.offset += block.size as u64;
        self.iterpos += 1;
        Some(buf)
    }
}

// ChunkMeta is the hash of a chunk of data and the size of the data, before hashing
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct ChunkMeta {
    hash: Hash,
    size: usize,
}

impl ChunkMeta {
    pub fn new(data: &[u8]) -> Self {
        let mh = hash::multihash(data);
        Self {
            hash: Hash::from(mh.to_bytes()),
            size: data.len(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.hash.to_bytes()
    }

    // TODO: consider removing this if not used
    pub fn read_from_disk<P: AsRef<Path>>(
        filepath: P,
    ) -> Result<Vec<Self>, Box<dyn std::error::Error>> {
        let chunker = Chunkerator::new(filepath, BLOCK_SIZE)?;
        let chunkmetas: Vec<Self> = chunker.into_iter().map(|e| Self::new(&e)).collect();
        Ok(chunkmetas)
    }
}

/// Chunkerator reads files a "chunk" at a time, and returns chunks via the
/// iterator.
#[derive(Debug)]
pub struct Chunkerator {
    filepath: PathBuf,
    buf_reader: BufReader<std::fs::File>,
}

impl Chunkerator {
    fn new<P: AsRef<Path>>(
        filepath: P,
        chunk_size: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let f = std::fs::File::open(filepath.as_ref()).unwrap();
        let reader = BufReader::with_capacity(chunk_size, f);
        Ok(Self {
            filepath: filepath.as_ref().to_path_buf(),
            buf_reader: reader,
        })
    }
}

impl std::iter::Iterator for Chunkerator {
    type Item = Vec<u8>;
    fn next(&mut self) -> Option<Self::Item> {
        // fill entire reader
        let data = match self.buf_reader.fill_buf() {
            Ok(data) => data,
            Err(e) => {
                error!("Chunkerator read error: {}", e);
                return None;
            }
        };
        // handle None case (no more data to read)
        if data.is_empty() {
            return None;
        }
        let data = data.to_vec();
        self.buf_reader.consume(data.len());
        Some(data)
    }
}

#[cfg(test)]
mod test {
    use super::{
        BlockRef, ChunkMeta, Chunkerator, FileRef, Hash, PlainBlockIndex, PlainFileIndex,
        BLOCK_SIZE,
    };
    use std::collections::{HashMap, HashSet};
    use std::path::Path;

    const TEST_BLOCKS_DIR_T1: &str = "test/blocks/t1/";
    // -rw-r--r-- 1 joshua staff 16384 Mar 22 15:32 file1.txt
    // -rw-r--r-- 1 joshua staff  4096 Mar 22 15:32 file2.txt
    // -rw-r--r-- 1 joshua staff  4096 Mar 22 15:32 file3.txt

    #[test]
    fn read_blocks() {
        let file1_path = Path::new(TEST_BLOCKS_DIR_T1).join("file1.txt");
        let chunk_metas1 = super::ChunkMeta::read_from_disk(file1_path).unwrap();
        assert_eq!(chunk_metas1, vec![
            ChunkMeta {
                hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                size: 4096,
            },
            ChunkMeta {
                hash: Hash::from("134089e75f89ca624a073a1b3648303a4abd77fd49325110aa08d683ea0a03de6f949650bbf74f33597f5dcc54c57aaeb47cd143452a320f06c69829c54dc7d9dbb5"),
                size: 4096,
            },
            ChunkMeta {
                hash: Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                size: 4096,
            },
            ChunkMeta {
                hash: Hash::from("1340854c0357e05ac2c579e0fac9e2f1be10e6f2e8e678bb0005592a60251d885ceda96764e3b75af33e53e204dc868a036c63354a6a402699e9b613a31a9c5b5549"),
                size: 4096,
            },
        ]);

        let file2_path = Path::new(TEST_BLOCKS_DIR_T1).join("file2.txt");
        let chunk_metas2 = super::ChunkMeta::read_from_disk(file2_path).unwrap();
        assert_eq!(chunk_metas2, vec![
                ChunkMeta {
                    hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                    size: 4096,
                },
            ],
        );

        let file3_path = Path::new(TEST_BLOCKS_DIR_T1).join("file3.txt");
        let chunk_metas3 = ChunkMeta::read_from_disk(file3_path).unwrap();
        // should be equal super::File objects
        assert_eq!(chunk_metas2, chunk_metas3);
    }

    #[test]
    fn file_index() {
        // build index and compare
        let index = PlainFileIndex::new(TEST_BLOCKS_DIR_T1).unwrap();

        let map: HashMap<Hash, FileRef> = HashMap::from([
            (
                Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                FileRef {
                    chunkmetas: vec![
                        ChunkMeta {
                            hash: Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                            size: 1024,
                        },
                    ],
                    paths: HashSet::from(["test/blocks/t1/file5.txt".into()])
                },
            ),
            (
                Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                FileRef {
                    chunkmetas: vec![
                        ChunkMeta {
                            hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from("134089e75f89ca624a073a1b3648303a4abd77fd49325110aa08d683ea0a03de6f949650bbf74f33597f5dcc54c57aaeb47cd143452a320f06c69829c54dc7d9dbb5"),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from("1340854c0357e05ac2c579e0fac9e2f1be10e6f2e8e678bb0005592a60251d885ceda96764e3b75af33e53e204dc868a036c63354a6a402699e9b613a31a9c5b5549"),
                            size: 4096,
                        },
                    ],
                    paths: HashSet::from(["test/blocks/t1/file1.txt".into()])
                },
            ),
            (
                Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                FileRef {
                        chunkmetas: vec![
                            ChunkMeta {
                               hash: Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                               size: 4096,
                            },
                        ],
                    paths: HashSet::from(["test/blocks/t1/file4.txt".into()])
                },
            ),
            (
                Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                FileRef {
                        chunkmetas: vec![
                            ChunkMeta {
                               hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                               size: 4096,
                            },
                        ],
                    paths: HashSet::from([
                        "test/blocks/t1/file2.txt".into(),
                        "test/blocks/t1/file3.txt".into(),
                    ])
                },
            ),
        ]);

        assert_eq!(index, PlainFileIndex { map });
    }

    #[test]
    fn block_index() {
        let file_index = PlainFileIndex::new(TEST_BLOCKS_DIR_T1).unwrap();
        let block_index = PlainBlockIndex::new(&file_index).unwrap();

        // there should be 5 distinct chunks in test dir
        assert_eq!(block_index.count_blocks(), 5);

        let map: HashMap<Hash, BlockRef> = HashMap::from([
            (
                Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                    ]),
                }
            ),
            (
                Hash::from("134089e75f89ca624a073a1b3648303a4abd77fd49325110aa08d683ea0a03de6f949650bbf74f33597f5dcc54c57aaeb47cd143452a320f06c69829c54dc7d9dbb5"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                    ]),
                },
            ),
            (
                Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                        Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                    ]),
                },
            ),
            (
                Hash::from("1340854c0357e05ac2c579e0fac9e2f1be10e6f2e8e678bb0005592a60251d885ceda96764e3b75af33e53e204dc868a036c63354a6a402699e9b613a31a9c5b5549"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                    ]),
                },
            ),
            (
                Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                        Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                    ]),
                },
            ),
        ]);

        assert_eq!(block_index, PlainBlockIndex { map });
    }

    #[test]
    fn chunkerator() {
        let file5_path = Path::new(TEST_BLOCKS_DIR_T1).join("file5.txt");
        let mut chunker = Chunkerator::new(file5_path, BLOCK_SIZE).unwrap();
        let chunk = chunker.next();
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().len(), 1024);
    }
}
