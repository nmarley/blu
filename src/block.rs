use crate::hash;
use crate::magic::Wizard;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const BLOCK_SIZE: usize = 4096;
use crate::chunkfile::ChunkFile;
use crate::hash::Hash;

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct PlainIndex {
    // file hash -> FileRef { file: File, paths: HashSet }
    map: HashMap<Hash, FileRef>,
}
impl PlainIndex {
    pub fn new<P: AsRef<Path> + std::fmt::Debug>(
        dir: P,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            map: Self::build_index(dir)?,
        })
    }

    fn build_index<P: AsRef<Path> + std::fmt::Debug>(
        dir: P,
    ) -> Result<HashMap<Hash, FileRef>, Box<dyn std::error::Error>> {
        let mut map: HashMap<Hash, FileRef> = HashMap::new();
        // Walkdir and all that ...
        let bludir = dir.as_ref().join(".blu/");
        for elem in WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()) {
            // dbg!(&elem);
            // skip special .blu dir
            if elem.path().starts_with(&bludir) {
                continue;
            }
            // TODO: allow symlinks?
            if !elem.file_type().is_file() {
                continue;
            }

            let file = File::read_from_disk(&elem.path())?;
            let file_hash = file.hash();
            let sth = map.entry(file_hash).or_insert(FileRef {
                file,
                paths: HashSet::new(),
            });
            sth.paths.insert(elem.into_path());
        }
        Ok(map)
    }
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct FileRef {
    file: File,
    paths: HashSet<PathBuf>,
}

#[derive(Debug, PartialEq, Clone, Hash, Serialize, Deserialize)]
pub struct Block {
    hash: Hash,
    size: usize,
}

impl Block {
    pub fn new(data: &[u8]) -> Self {
        let mh = hash::multihash(data);
        Self {
            hash: Hash::from(mh.to_bytes()),
            size: data.len(),
        }
    }

    pub fn hash(&self) -> Vec<u8> {
        self.hash.to_bytes()
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct File {
    blocks: Vec<Block>,
    filetype: String, // TODO: ref table?
}

// == encrypted parts

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct ChunkFileLocation {
    path: PathBuf,
    index: usize,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Default)]
pub struct ChunkFileIndex {
    // map the encrypted hash to the location of the data on disk
    map: HashMap<Hash, ChunkFileLocation>,
}

impl ChunkFileIndex {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn add_chunk_location(&mut self, chunk_hash: &[u8], location: &ChunkFileLocation) {
        self.map
            .insert(chunk_hash.to_vec().into(), location.clone());
    }

    // returns the encrypted from disk, decrypt it yourself
    //
    // TODO: seems REALLY weird to just open a new ChunkFile on disk every time
    // to read a single block ... should we maintain a map of open files for
    // reading the chunks? e.g. once this particular location is opened, we
    // don't close it, keep it open at least for X most recently accessed
    // files?
    pub fn get_enc_block(&self, hash: &Hash) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let enc_location = self.map.get(hash).ok_or("location not found")?;

        let mut f = fs::File::open(&enc_location.path)?;
        let mut chunkdata = Vec::new();
        let _bytes_read = f.read(&mut chunkdata)?;
        let chunkfile = ChunkFile::deserialize(&chunkdata)?;

        chunkfile.get_chunk(enc_location.index)
    }
}

// == end encrypted parts

impl File {
    pub fn hash(&self) -> Hash {
        let mut all_hashes: Vec<u8> = vec![];
        for block in self.blocks.iter() {
            let mut block_hash = block.hash();
            all_hashes.append(&mut block_hash);
        }

        Hash::from(hash::multihash(&all_hashes).to_bytes())
    }

    pub fn read_from_disk<P: AsRef<Path> + std::fmt::Debug>(
        filepath: P,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // for file magic
        let wiz = Wizard::new();

        let f = std::fs::File::open(filepath).unwrap();
        let mut reader = BufReader::with_capacity(BLOCK_SIZE, f);
        let mut blocks: Vec<Block> = vec![];
        let mut count: usize = 0;
        let mut filetype: String = "".to_string();

        while let Ok(data) = reader.fill_buf() {
            if data.is_empty() {
                break;
            }
            let actual_data = data.to_vec();
            blocks.push(Block::new(&actual_data));
            reader.consume(actual_data.len());

            // TODO: this but better
            if count == 0 {
                filetype = wiz
                    .get_filetype(&actual_data, actual_data.len())
                    .unwrap_or_else(|_| "other".into());
            }

            count += 1;
        }

        Ok(Self { blocks, filetype })
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
    use super::{Block, File, Hash, PlainIndex};
    use std::path::Path;

    const TEST_BLOCKS_DIR_T1: &str = "test/blocks/t1/";
    // -rw-r--r-- 1 joshua staff 16384 Mar 22 15:32 file1.txt
    // -rw-r--r-- 1 joshua staff  4096 Mar 22 15:32 file2.txt
    // -rw-r--r-- 1 joshua staff  4096 Mar 22 15:32 file3.txt

    #[test]
    fn read_blocks() {
        let file1_path = Path::new(TEST_BLOCKS_DIR_T1).join("file1.txt");
        let file1 = super::File::read_from_disk(file1_path).unwrap();
        assert_eq!(file1.hash().to_bytes(), hex::decode("13407a025c8c4b81348ee26290ae55485822cd48bc29edfeaf6b762a7860758cb5f0317243a701f21558bfb3b81762d50d296020e559dda1a58f25f52204b430ab64").unwrap());
        assert_eq!(file1, File {
            blocks: vec![
                Block {
                    hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                    size: 4096,
                },
                Block {
                    hash: Hash::from("134089e75f89ca624a073a1b3648303a4abd77fd49325110aa08d683ea0a03de6f949650bbf74f33597f5dcc54c57aaeb47cd143452a320f06c69829c54dc7d9dbb5"),
                    size: 4096,
                },
                Block {
                    hash: Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                    size: 4096,
                },
                Block {
                    hash: Hash::from("1340854c0357e05ac2c579e0fac9e2f1be10e6f2e8e678bb0005592a60251d885ceda96764e3b75af33e53e204dc868a036c63354a6a402699e9b613a31a9c5b5549"),
                    size: 4096,
                },
            ],
            filetype: "ASCII text, with very long lines (1024), with no line terminators".to_string(),
        });

        let file2_path = Path::new(TEST_BLOCKS_DIR_T1).join("file2.txt");
        let file2 = super::File::read_from_disk(file2_path).unwrap();
        assert_eq!(file2.hash().to_bytes(), hex::decode("1340931e4b89c108f368b4070efc34c7e38b19b279e388f9fa4f96225ddb785bbaca7e2a38e2b81748100a7169aee58d82cc8df842cdc8f07785f0fc45c7fd567dd5").unwrap());
        assert_eq!(file2, File {
            blocks: vec![
                Block {
                    hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                    size: 4096,
                },
            ],
            filetype: "ASCII text, with very long lines (1024), with no line terminators".to_string(),
        });

        let file3_path = Path::new(TEST_BLOCKS_DIR_T1).join("file3.txt");
        let file3 = File::read_from_disk(file3_path).unwrap();
        assert_eq!(file3.hash().to_bytes(), hex::decode("1340931e4b89c108f368b4070efc34c7e38b19b279e388f9fa4f96225ddb785bbaca7e2a38e2b81748100a7169aee58d82cc8df842cdc8f07785f0fc45c7fd567dd5").unwrap());
        // should be equal super::File objects
        assert_eq!(file2, file3);
    }

    use crate::block::FileRef;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn block_index() {
        // build index and compare
        let index = PlainIndex::new(TEST_BLOCKS_DIR_T1).unwrap();

        let map: HashMap<Hash, FileRef> = HashMap::from([
            (
                Hash::from("1340b62f901a22f1e06883626f66af5660f8510ce6352115bf8511d648a99e8a69936277dc39afb1ae80154d923ab396bcd0d8dce7744b6df5d287e0566ace86b9f4"),
                FileRef {
                    file: File {
                        blocks: vec![
                            Block {
                                hash: Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                                size: 1024,
                            },
                        ],
                        filetype: "ASCII text, with very long lines (1023)".into(),
                    },
                    paths: HashSet::from(["test/blocks/t1/file5.txt".into()])
                },
            ),
            (
                Hash::from("13407a025c8c4b81348ee26290ae55485822cd48bc29edfeaf6b762a7860758cb5f0317243a701f21558bfb3b81762d50d296020e559dda1a58f25f52204b430ab64"),
                FileRef {
                    file: File {
                        blocks: vec![
                            Block {
                               hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                               size: 4096,
                            },
                            Block {
                                hash: Hash::from("134089e75f89ca624a073a1b3648303a4abd77fd49325110aa08d683ea0a03de6f949650bbf74f33597f5dcc54c57aaeb47cd143452a320f06c69829c54dc7d9dbb5"),
                                size: 4096,
                            },
                            Block {
                                hash: Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                                size: 4096,
                            },
                            Block {
                                hash: Hash::from("1340854c0357e05ac2c579e0fac9e2f1be10e6f2e8e678bb0005592a60251d885ceda96764e3b75af33e53e204dc868a036c63354a6a402699e9b613a31a9c5b5549"),
                                size: 4096,
                            },
                        ],
                        filetype: "ASCII text, with very long lines (1024), with no line terminators".into(),
                    },
                    paths: HashSet::from(["test/blocks/t1/file1.txt".into()])
                },
            ),
            (
                Hash::from("134086dd2fbbbfa83556d52a38b54107231b96cd6c6dcce2e12857e2eb75e6ddbee69b53c8f1aa5e48db57a1cb4eeaff7499d91a8daea7e4c11bc82808d9543dad5d"),
                FileRef {
                    file: File {
                        blocks: vec![
                            Block {
                               hash: Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                               size: 4096,
                            },
                        ],
                        filetype: "ASCII text, with very long lines (1024), with no line terminators".into(),
                    },
                    paths: HashSet::from(["test/blocks/t1/file4.txt".into()])
                },
            ),
            (
                Hash::from("1340931e4b89c108f368b4070efc34c7e38b19b279e388f9fa4f96225ddb785bbaca7e2a38e2b81748100a7169aee58d82cc8df842cdc8f07785f0fc45c7fd567dd5"),
                FileRef {
                    file: File {
                        blocks: vec![
                            Block {
                               hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                               size: 4096,
                            },
                        ],
                        filetype: "ASCII text, with very long lines (1024), with no line terminators".into(),
                    },
                    paths: HashSet::from([
                        "test/blocks/t1/file2.txt".into(),
                        "test/blocks/t1/file3.txt".into(),
                    ])
                },
            ),
        ]);

        assert_eq!(index, PlainIndex { map });
    }
}
