use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::hash::{self, Hash};
use crate::magic::Wizard;

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

            let file = VecChunkMeta::read_from_disk(&elem.path())?;
            let file_hash = file.hash();
            let fileref = map.entry(file_hash).or_insert_with(|| FileRef::new(file));
            fileref.paths.insert(elem.into_path());
        }
        Ok(map)
    }

    pub fn map_ref(&self) -> &HashMap<Hash, FileRef> {
        &self.map
    }
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct PlainBlockIndex {
    // plain block hash -> BlockRef
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
            for block in fr.file.chunkmetas.iter() {
                let blockref = block_index
                    .entry(block.hash.clone())
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
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct BlockRef {
    // TBD: this has to be integrated w/the ChunkFileIndex and encryptor
    //
    encrypted_hash: Option<Hash>,

    // hashes of the files which reference this block
    referencing_file_hashes: HashSet<Hash>,
}

impl BlockRef {
    fn new() -> Self {
        Self {
            encrypted_hash: None,
            referencing_file_hashes: HashSet::new(),
        }
    }
}
impl Default for BlockRef {
    fn default() -> Self {
        Self::new()
    }
}

/// FileRef is a container encapsulating a VecChunkMeta object (collection of
/// hashes of chunks read from a fs::File) and filesystem references to it
/// (filenames)
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct FileRef {
    file: VecChunkMeta,
    paths: HashSet<PathBuf>,
}

impl FileRef {
    pub fn new(f: VecChunkMeta) -> Self {
        Self {
            file: f,
            paths: HashSet::new(),
        }
    }

    pub fn iter(&self) -> Result<FileRefIterator, Box<dyn std::error::Error>> {
        if self.paths.is_empty() {
            return Err("no path from which to read bytes".into());
        }
        let path = self.paths.iter().next().unwrap();
        Ok(FileRefIterator::new(self.file.clone(), path.to_path_buf()))
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct FileRefIterator {
    file: VecChunkMeta,
    path: PathBuf,
    iterpos: usize,
    offset: u64,
}
impl FileRefIterator {
    pub fn new(f: VecChunkMeta, path: PathBuf) -> Self {
        Self {
            file: f,
            path,
            iterpos: 0,
            offset: 0,
        }
    }
}

impl std::iter::Iterator for FileRefIterator {
    type Item = Vec<u8>;
    fn next(&mut self) -> Option<Self::Item> {
        // dbg!(&self.iterpos);
        // dbg!(&self.offset);
        // dbg!(&self.path);
        // dbg!(self.file.chunkmetas.len());

        if self.iterpos >= self.file.chunkmetas.len() {
            return None;
        }
        let block = &self.file.chunkmetas[self.iterpos];

        // read block.size bytes
        let mut f = std::fs::File::open(&self.path).expect("wtf?");
        let mut buf: Vec<u8> = vec![0u8; block.size];
        let _seeko = f.seek(SeekFrom::Start(self.offset)).expect("wtf2?");
        // dbg!(&seeko);
        f.read_exact(&mut buf).expect("wtf3");

        self.offset += block.size as u64;
        self.iterpos += 1;
        Some(buf)
        // None
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
}

/// VecChunkMeta is a collection of ChunkMeta
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct VecChunkMeta {
    chunkmetas: Vec<ChunkMeta>,
    // filetype: String, // TODO: ref table?
}

impl VecChunkMeta {
    pub fn hash(&self) -> Hash {
        let mut all_hashes: Vec<u8> = vec![];
        for cm in self.chunkmetas.iter() {
            let mut chunk_hash_bytes = cm.to_bytes();
            all_hashes.append(&mut chunk_hash_bytes);
        }

        Hash::from(hash::multihash(&all_hashes).to_bytes())
    }

    pub fn read_from_disk<P: AsRef<Path>>(filepath: P) -> Result<Self, Box<dyn std::error::Error>> {
        // for file magic
        // let wiz = Wizard::new();

        let f = std::fs::File::open(filepath).unwrap();
        let mut reader = BufReader::with_capacity(BLOCK_SIZE, f);
        let mut chunkmetas: Vec<ChunkMeta> = vec![];
        let mut count: usize = 0;
        let mut filetype: String = "".to_string();

        while let Ok(data) = reader.fill_buf() {
            if data.is_empty() {
                break;
            }
            let actual_data = data.to_vec();
            chunkmetas.push(ChunkMeta::new(&actual_data));
            reader.consume(actual_data.len());

            // // TODO: this but better
            // if count == 0 {
            //     filetype = wiz
            //         .get_filetype(&actual_data, actual_data.len())
            //         .unwrap_or_else(|_| "other".into());
            // }

            count += 1;
        }

        Ok(Self {
            chunkmetas,
            // filetype,
        })
    }
}

// pub struct BlockReader {
// }
// impl Iterator for BlockReader {
//     type Item = Block;
// }

// fn read_blocks_f<sRef<dyn BufRead>>(reader: R) -> Vec<Vec<u8>> {
//     let mut blocks: Vec<Vec<u8>> = vec![];
//     let mut r = reader.as_ref();
//     // mut dyn BufRead
//     let mut size = 1;
//     while size > 0 {
//         let data = r.fill_buf().unwrap();
//         size = data.len();
//         r.consume(size);
//         blocks.push(data.to_vec());
//     }
//     blocks
//     // let mut reader = BufReader::with_capacity(BLOCK_SIZE, file);
//     // dbg!(&BLOCK_SIZE);
//     // let block_bytes = reader.fill_buf().unwrap();
//     // dbg!(&block_bytes.len());
// }

// pub struct Entry {
//     paths: HashSet<PathBuf>,
//     hash: Vec<u8>,
//     size: u64,
//     enc: Option<Encrypted>,
//     tags: Vec<String>,     // TODO: proper tagging, or... ?
//     notes: Option<String>, // free-form text
// }

#[cfg(test)]
mod test {
    use super::{
        BlockRef, ChunkMeta, FileRef, Hash, PlainBlockIndex, PlainFileIndex, VecChunkMeta,
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
        let file1 = super::VecChunkMeta::read_from_disk(file1_path).unwrap();
        assert_eq!(file1.hash().to_bytes(), hex::decode("13407a025c8c4b81348ee26290ae55485822cd48bc29edfeaf6b762a7860758cb5f0317243a701f21558bfb3b81762d50d296020e559dda1a58f25f52204b430ab64").unwrap());
        assert_eq!(file1, VecChunkMeta {
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
            // filetype: "ASCII text, with very long lines (1024), with no line terminators".to_string(),
        });

        let file2_path = Path::new(TEST_BLOCKS_DIR_T1).join("file2.txt");
        let file2 = super::VecChunkMeta::read_from_disk(file2_path).unwrap();
        assert_eq!(file2.hash().to_bytes(), hex::decode("1340931e4b89c108f368b4070efc34c7e38b19b279e388f9fa4f96225ddb785bbaca7e2a38e2b81748100a7169aee58d82cc8df842cdc8f07785f0fc45c7fd567dd5").unwrap());
        assert_eq!(file2, VecChunkMeta {
            chunkmetas: vec![
                ChunkMeta {
                    hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                    size: 4096,
                },
            ],
            // filetype: "ASCII text, with very long lines (1024), with no line terminators".to_string(),
        });

        let file3_path = Path::new(TEST_BLOCKS_DIR_T1).join("file3.txt");
        let file3 = VecChunkMeta::read_from_disk(file3_path).unwrap();
        assert_eq!(file3.hash().to_bytes(), hex::decode("1340931e4b89c108f368b4070efc34c7e38b19b279e388f9fa4f96225ddb785bbaca7e2a38e2b81748100a7169aee58d82cc8df842cdc8f07785f0fc45c7fd567dd5").unwrap());
        // should be equal super::File objects
        assert_eq!(file2, file3);
    }

    #[test]
    fn file_index() {
        // build index and compare
        let index = PlainFileIndex::new(TEST_BLOCKS_DIR_T1).unwrap();

        let map: HashMap<Hash, FileRef> = HashMap::from([
            (
                Hash::from("1340b62f901a22f1e06883626f66af5660f8510ce6352115bf8511d648a99e8a69936277dc39afb1ae80154d923ab396bcd0d8dce7744b6df5d287e0566ace86b9f4"),
                FileRef {
                    file: VecChunkMeta {
                        chunkmetas: vec![
                            ChunkMeta {
                                hash: Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                                size: 1024,
                            },
                        ],
                        // filetype: "ASCII text, with very long lines (1023)".into(),
                    },
                    paths: HashSet::from(["test/blocks/t1/file5.txt".into()])
                },
            ),
            (
                Hash::from("13407a025c8c4b81348ee26290ae55485822cd48bc29edfeaf6b762a7860758cb5f0317243a701f21558bfb3b81762d50d296020e559dda1a58f25f52204b430ab64"),
                FileRef {
                    file: VecChunkMeta {
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
                        // filetype: "ASCII text, with very long lines (1024), with no line terminators".into(),
                    },
                    paths: HashSet::from(["test/blocks/t1/file1.txt".into()])
                },
            ),
            (
                Hash::from("134086dd2fbbbfa83556d52a38b54107231b96cd6c6dcce2e12857e2eb75e6ddbee69b53c8f1aa5e48db57a1cb4eeaff7499d91a8daea7e4c11bc82808d9543dad5d"),
                FileRef {
                    file: VecChunkMeta {
                        chunkmetas: vec![
                            ChunkMeta {
                               hash: Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                               size: 4096,
                            },
                        ],
                        // filetype: "ASCII text, with very long lines (1024), with no line terminators".into(),
                    },
                    paths: HashSet::from(["test/blocks/t1/file4.txt".into()])
                },
            ),
            (
                Hash::from("1340931e4b89c108f368b4070efc34c7e38b19b279e388f9fa4f96225ddb785bbaca7e2a38e2b81748100a7169aee58d82cc8df842cdc8f07785f0fc45c7fd567dd5"),
                FileRef {
                    file: VecChunkMeta {
                        chunkmetas: vec![
                            ChunkMeta {
                               hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                               size: 4096,
                            },
                        ],
                        // filetype: "ASCII text, with very long lines (1024), with no line terminators".into(),
                    },
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
                        Hash::from("1340b62f901a22f1e06883626f66af5660f8510ce6352115bf8511d648a99e8a69936277dc39afb1ae80154d923ab396bcd0d8dce7744b6df5d287e0566ace86b9f4"),
                    ]),
                    encrypted_hash: None,
                }
            ),
            (
                Hash::from("134089e75f89ca624a073a1b3648303a4abd77fd49325110aa08d683ea0a03de6f949650bbf74f33597f5dcc54c57aaeb47cd143452a320f06c69829c54dc7d9dbb5"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("13407a025c8c4b81348ee26290ae55485822cd48bc29edfeaf6b762a7860758cb5f0317243a701f21558bfb3b81762d50d296020e559dda1a58f25f52204b430ab64"),
                    ]),
                    encrypted_hash: None,
                },
            ),
            (
                Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("13407a025c8c4b81348ee26290ae55485822cd48bc29edfeaf6b762a7860758cb5f0317243a701f21558bfb3b81762d50d296020e559dda1a58f25f52204b430ab64"),
                        Hash::from("1340931e4b89c108f368b4070efc34c7e38b19b279e388f9fa4f96225ddb785bbaca7e2a38e2b81748100a7169aee58d82cc8df842cdc8f07785f0fc45c7fd567dd5"),
                    ]),
                    encrypted_hash: None,
                },
            ),
            (
                Hash::from("1340854c0357e05ac2c579e0fac9e2f1be10e6f2e8e678bb0005592a60251d885ceda96764e3b75af33e53e204dc868a036c63354a6a402699e9b613a31a9c5b5549"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("13407a025c8c4b81348ee26290ae55485822cd48bc29edfeaf6b762a7860758cb5f0317243a701f21558bfb3b81762d50d296020e559dda1a58f25f52204b430ab64"),
                    ]),
                    encrypted_hash: None,
                },
            ),
            (
                Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                BlockRef {
                    referencing_file_hashes: HashSet::from([
                        Hash::from("134086dd2fbbbfa83556d52a38b54107231b96cd6c6dcce2e12857e2eb75e6ddbee69b53c8f1aa5e48db57a1cb4eeaff7499d91a8daea7e4c11bc82808d9543dad5d"),
                        Hash::from("13407a025c8c4b81348ee26290ae55485822cd48bc29edfeaf6b762a7860758cb5f0317243a701f21558bfb3b81762d50d296020e559dda1a58f25f52204b430ab64"),
                    ]),
                    encrypted_hash: None,
                },
            ),
        ]);

        assert_eq!(block_index, PlainBlockIndex { map });
    }
}
