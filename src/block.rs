use multihash::{Code, Hasher, MultihashDigest, Sha2_512};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::hash::{self, Hash};

const BLOCK_SIZE: usize = 4096;

/// PlainIndex ...
#[derive(Default, Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct PlainIndex {
    // file hash -> FileRef { file: File, paths: HashSet }
    files: HashMap<Hash, FileRef>,

    // plain block hash -> BlockRef
    blocks: HashMap<Hash, BlockRef>,
}

type FileIndex = HashMap<Hash, FileRef>;
type BlockIndex = HashMap<Hash, BlockRef>;

impl PlainIndex {
    pub fn new<P: AsRef<Path>>(dir: P) -> Result<Self, Box<dyn std::error::Error>> {
        let (files, blocks) = Self::build_index(dir)?;
        Ok(Self { files, blocks })
    }

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
                blockref.referencing_file_hashes.insert(file_hash.clone());
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

// blockref -> option<enc hash>
//          -> set of references to chunk on disk
/// BlockRef has a collection of file hashes which reference a particular block.
#[derive(Default, Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct BlockRef {
    // hashes of the files which reference this block
    // this field is now entirely redundant b/c of references below.
    // can be removed any time.
    pub referencing_file_hashes: HashSet<Hash>,
    // on-disk locations where this block can be read if necessary
    pub references: HashSet<FileRefLocationIndex>,
}

/// FileRefLocationIndex gives the location of a chunk within a FileRef
/// (identified by file hash), with a byte offset and number of bytes to be
/// read.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Ord, PartialOrd, Eq, Hash)]
pub struct FileRefLocationIndex {
    pub file_hash: Hash,
    pub offset: usize,
    pub size: usize,
}

impl BlockRef {
    fn new() -> Self {
        Self {
            referencing_file_hashes: HashSet::new(),
            references: HashSet::new(),
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

    pub fn get_a_path(&self) -> PathBuf {
        self.paths.iter().next().unwrap().to_path_buf()
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
    buf_reader: BufReader<std::fs::File>,
}

impl Chunkerator {
    fn new<P: AsRef<Path>>(
        filepath: P,
        chunk_size: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let f = std::fs::File::open(filepath.as_ref()).unwrap();
        let reader = BufReader::with_capacity(chunk_size, f);
        Ok(Self { buf_reader: reader })
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
        BlockRef, ChunkMeta, Chunkerator, FileRef, FileRefLocationIndex, Hash, PlainIndex,
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

    fn helper_files_map() -> HashMap<Hash, FileRef> {
        HashMap::from([
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
        ])
    }

    fn helper_blocks_map() -> HashMap<Hash, BlockRef> {
        HashMap::from([
            (
                Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                    ]),
                    references: HashSet::from([
                        FileRefLocationIndex {
                            file_hash: Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                            offset: 0,
                            size: 1024,
                        },
                    ]),
                }
            ),
            (
                Hash::from("134089e75f89ca624a073a1b3648303a4abd77fd49325110aa08d683ea0a03de6f949650bbf74f33597f5dcc54c57aaeb47cd143452a320f06c69829c54dc7d9dbb5"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                    ]),
                    references: HashSet::from([
                        FileRefLocationIndex {
                            file_hash: Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                            offset: 4096,
                            size: 4096,
                        },
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
                    references: HashSet::from([
                        FileRefLocationIndex {
                            file_hash: Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                            offset: 0,
                            size: 4096,
                        },
                        FileRefLocationIndex {
                            file_hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                            offset: 0,
                            size: 4096,
                        },
                    ]),
                },
            ),
            (
                Hash::from("1340854c0357e05ac2c579e0fac9e2f1be10e6f2e8e678bb0005592a60251d885ceda96764e3b75af33e53e204dc868a036c63354a6a402699e9b613a31a9c5b5549"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                    ]),
                    references: HashSet::from([
                        FileRefLocationIndex {
                            file_hash: Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                            offset: 12288,
                            size: 4096,
                        },
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
                    references: HashSet::from([
                        FileRefLocationIndex {
                            file_hash: Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                            offset: 8192,
                            size: 4096,
                        },
                        FileRefLocationIndex {
                            file_hash: Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                            offset: 0,
                            size: 4096,
                        },
                    ]),
                },
            ),
        ])
    }

    #[test]
    fn indexes() {
        let index = PlainIndex::new(TEST_BLOCKS_DIR_T1).unwrap();
        assert_eq!(index.files, helper_files_map());
        assert_eq!(index.blocks, helper_blocks_map());
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
