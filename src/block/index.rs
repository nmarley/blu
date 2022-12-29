use chrono::NaiveDateTime;
use multihash::{Code, Hasher, MultihashDigest, Sha2_512};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::age::BlackBox;
use crate::compression::{compress, decompress};
use crate::format::datetime_format;
use crate::hash::Hash;

use super::blockref::{BlockRef, FileRefLocationIndex};
use super::ChunkMeta;
use super::Chunkerator;
use super::FileRef;
// use super::fileref::FileRef;

const BLOCK_SIZE: usize = 4096;

pub const INDEX_FILENAME: &str = "index.dat";
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

    // read / write serialization methods integrate BlackBox for automagic
    // decryption / encryption when reading from disk
    pub fn write<W: io::Write>(
        &self,
        mut stream: W,
        bbox: &BlackBox,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let serialized = self.serialize()?;
        let compressed = compress(&serialized)?;
        let encrypted = bbox.encrypt(&compressed)?;
        let _ = stream.write_all(&encrypted);
        Ok(())
    }

    fn deserialize(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        // let decoded: Index = serde_cbor::from_slice(data)?;
        let decoded: Self = bincode::deserialize(data)?;
        // let decoded: Self = match bincode::deserialize(data) {
        //     Ok(index) => index,
        //     Err(_) => OldIndex::deserialize(data)?.into_index(),
        // };
        Ok(decoded)
    }

    fn serialize(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let encoded: Vec<u8> = bincode::serialize(&self)?;
        // let encoded: Vec<u8> = serde_cbor::to_vec(&self)?;
        Ok(encoded)
    }

    pub fn read<R: io::Read>(
        mut stream: R,
        bbox: &BlackBox,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut encrypted = Vec::new();
        let _ = stream.read_to_end(&mut encrypted)?;
        let compressed = bbox.decrypt(&encrypted)?;
        let serialized = decompress(&compressed)?;
        Self::deserialize(&serialized)
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
            #[allow(clippy::needless_borrow)]
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
            let chunker = Chunkerator::new(elem.path(), BLOCK_SIZE)?;
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

    // Update the index, return a list of removed (dangling) entries
    // TODO: test this update method
    //   - ensure that pathbufs are updated
    //   - ensure that deleted filerefs and blockrefs are removed from index
    //   - ensure that added filerefs and blockrefs are added to index
    //
    // Probably should create an index via `new`, and then use a different
    // updated dir with `update` and ensure expected changes are applied.
    pub fn update<P: AsRef<Path>>(
        &mut self,
        base_dir: P,
    ) -> Result<(Vec<FileRef>, Vec<BlockRef>), Box<dyn std::error::Error>> {
        let new_index = Self::new(base_dir)?;

        let mut to_delete: HashSet<Hash> = HashSet::new();
        let mut new_paths: HashMap<Hash, HashSet<PathBuf>> = HashMap::new();
        let mut is_updated = false;

        // TODO: handle both BlockRefs and FileRefs in NEW and OLD

        // for each fileref in OLD ...
        for hash in self.files.keys() {
            if let Some(fileref) = new_index.files.get(hash) {
                // TODO: WRITE TEST FOR THIS
                // update in case path changed or was added
                new_paths.insert(hash.clone(), fileref.paths.clone());
            } else {
                // if it does NOT exist in NEW ...
                // ... add it to to_delete
                to_delete.insert(hash.clone());
            }
        }

        // set new paths
        for (hash, paths) in new_paths.into_iter() {
            self.files.entry(hash).and_modify(|e| e.paths = paths);
        }

        // for each hash/fileref in NEW ...
        for (hash, fileref) in new_index.files.into_iter() {
            if self.files.get(&hash).is_none() {
                // add it
                self.files.insert(hash, fileref);
            }
        }

        // files HashMap::<Hash, FileRef>
        // to_delete HashSet<&Hash>
        let mut deleted_filerefs: Vec<FileRef> = vec![];
        for hash in to_delete.into_iter() {
            let e = self.files.remove_entry(&hash).unwrap();
            deleted_filerefs.push(e.1);
            is_updated = true;
        }

        // blockrefs
        //
        let mut to_delete: HashSet<Hash> = HashSet::new();
        // for each blockref in OLD ...
        for hash in self.blocks.keys() {
            if new_index.blocks.get(hash).is_none() {
                // this blockref should be removed
                // ... add it to to_delete
                to_delete.insert(hash.clone());
            }
        }

        // blocks HashMap::<Hash, BlockRef>
        // to_delete HashSet<&Hash>
        let mut deleted_blockrefs: Vec<BlockRef> = vec![];
        for hash in to_delete.into_iter() {
            let e = self.blocks.remove_entry(&hash).unwrap();
            deleted_blockrefs.push(e.1);
            is_updated = true;
        }

        if is_updated {
            self.updated_at = now();
        }

        Ok((deleted_filerefs, deleted_blockrefs))
    }
}

fn now() -> chrono::NaiveDateTime {
    // returns a NaiveDateTime without milli/nano seconds
    NaiveDateTime::from_timestamp(chrono::Utc::now().timestamp(), 0)
}

#[cfg(test)]
mod test {
    use std::collections::{HashMap, HashSet};
    use std::path::Path;

    use super::{BlockRef, ChunkMeta, FileRef, FileRefLocationIndex, PlainIndex};
    use crate::hash::Hash;

    const TEST_BLOCKS_DIR_T5: &str = "test/blocks/t5/";

    #[test]
    fn update_index() {
        let before_path = Path::new(TEST_BLOCKS_DIR_T5).join("before");
        let after_path = Path::new(TEST_BLOCKS_DIR_T5).join("after");
        let mut index = PlainIndex::new(before_path).unwrap();
        // let exp = helper_files_map("test/blocks/t5/before");

        // TODO: remove this block once test complete
        // for (hash, fileref) in index.files.iter() {
        //     if let Some(fr) = exp.get(hash) {
        //         assert_eq!(fr, fileref);
        //     }
        //     // println!("{:?}: {:?}", hash, fileref);
        // }

        let prefix = "test/blocks/t5/before";
        let before_filerefs = HashMap::from([
            (
                Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                FileRef {
                    chunkmetas: vec![
                        ChunkMeta {
                            hash: Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                            size: 1024,
                        },
                    ],
                    paths: HashSet::from([format!("{}/file5.txt", prefix).into()])
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
                    paths: HashSet::from([format!("{}/file1.txt", prefix).into()])
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
                    paths: HashSet::from([format!("{}/file4.txt", prefix).into()])
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
                        format!("{}/file2.txt", prefix).into(),
                        format!("{}/file3.txt", prefix).into(),
                    ])
                },
            ),
        ]);

        let before_blockrefs = HashMap::from([
            (
                Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                BlockRef {
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
        ]);

        let prefix = "test/blocks/t5/after";
        let after_filerefs = HashMap::from([
            (
                Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                FileRef {
                    chunkmetas: vec![
                        ChunkMeta {
                            hash: Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                            size: 1024,
                        },
                    ],
                    paths: HashSet::from([format!("{}/file6.txt", prefix).into()])
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
                    paths: HashSet::from([format!("{}/file1.txt", prefix).into()])
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
                        format!("{}/file2.txt", prefix).into(),
                    ])
                },
            ),
        ]);

        // let after_blockrefs = HashMap::from([
        //     (
        //         Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
        //         BlockRef {
        //             references: HashSet::from([
        //                 FileRefLocationIndex {
        //                     file_hash: Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
        //                     offset: 0,
        //                     size: 1024,
        //                 },
        //             ]),
        //         }
        //     ),
        //     (
        //         Hash::from("134089e75f89ca624a073a1b3648303a4abd77fd49325110aa08d683ea0a03de6f949650bbf74f33597f5dcc54c57aaeb47cd143452a320f06c69829c54dc7d9dbb5"),
        //         BlockRef {
        //             references: HashSet::from([
        //                 FileRefLocationIndex {
        //                     file_hash: Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
        //                     offset: 4096,
        //                     size: 4096,
        //                 },
        //             ]),
        //         },
        //     ),
        //     (
        //         Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
        //         BlockRef {
        //             references: HashSet::from([
        //                 FileRefLocationIndex {
        //                     file_hash: Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
        //                     offset: 0,
        //                     size: 4096,
        //                 },
        //                 FileRefLocationIndex {
        //                     file_hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
        //                     offset: 0,
        //                     size: 4096,
        //                 },
        //             ]),
        //         },
        //     ),
        //     (
        //         Hash::from("1340854c0357e05ac2c579e0fac9e2f1be10e6f2e8e678bb0005592a60251d885ceda96764e3b75af33e53e204dc868a036c63354a6a402699e9b613a31a9c5b5549"),
        //         BlockRef {
        //             references: HashSet::from([
        //                 FileRefLocationIndex {
        //                     file_hash: Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
        //                     offset: 12288,
        //                     size: 4096,
        //                 },
        //             ]),
        //         },
        //     ),
        //     (
        //         Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
        //         BlockRef {
        //             references: HashSet::from([
        //                 FileRefLocationIndex {
        //                     file_hash: Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
        //                     offset: 8192,
        //                     size: 4096,
        //                 },
        //                 FileRefLocationIndex {
        //                     file_hash: Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
        //                     offset: 0,
        //                     size: 4096,
        //                 },
        //             ]),
        //         },
        //     ),
        // ]);

        assert_eq!(index.files, before_filerefs);
        assert_eq!(index.blocks, before_blockrefs);

        // ├── after
        // │   ├── file1.txt
        // │   ├── file2.txt
        // │   └── file6.txt
        // └── before
        //     ├── file1.txt
        //     ├── file2.txt
        //     ├── file3.txt
        //     ├── file4.txt
        //     └── file5.txt

        let (filerefs, _blockrefs) = index.update(after_path).unwrap();
        assert_eq!(index.files, after_filerefs);
        // assert_eq!(index.blocks, after_blockrefs);

        let prefix = "test/blocks/t5/before";
        let deleted_filerefs = Vec::from([
                FileRef {
                        chunkmetas: vec![
                            ChunkMeta {
                               hash: Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                               size: 4096,
                            },
                        ],
                    paths: HashSet::from([format!("{}/file4.txt", prefix).into()])
                },
        ]);

        // let deleted_blockrefs = HashMap::from([
        //     (
        //         Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
        //         FileRef {
        //                 chunkmetas: vec![
        //                     ChunkMeta {
        //                        hash: Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
        //                        size: 4096,
        //                     },
        //                 ],
        //             paths: HashSet::from([format!("{}/file4.txt", prefix).into()])
        //         },
        //     ),
        // ]);

        assert_eq!(deleted_filerefs, filerefs);
        // assert_eq!(deleted_blockrefs, blockrefs);
    }
}
