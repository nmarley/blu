mod blockref;
mod chunkerator;
mod chunkmeta;
mod fileref;
mod index;

pub(crate) use blockref::BlockRef;
pub use chunkerator::chunk_bytes;
pub use chunkerator::Chunkerator;
pub use chunkmeta::ChunkMeta;
pub use fileref::FileRef;
pub use index::PlainIndex;
pub use index::CURRENT_INDEX_VERSION;
pub use index::INDEX_FILENAME;

/// Block size in bytes, most filesystems use 4k blocks
pub const BLOCK_SIZE: usize = 4096;
/// Default chunk size for encrypting+indexing, should be a multiple of block
/// size. 512 KiB aligns with EBS snapshot changed-block granularity.
pub const DEFAULT_CHUNK_SIZE: usize = BLOCK_SIZE << 7;

#[cfg(test)]
mod test {
    use std::collections::{HashMap, HashSet};
    use std::path::Path;

    use super::blockref::BlockRef;
    use super::{ChunkMeta, Chunkerator, FileRef, PlainIndex};
    use crate::error::BluError;
    use crate::hash::Hash;
    use crate::io::Position;

    const TEST_BLOCKS_DIR_T1: &str = "test/blocks/t1/";
    // -rw-r--r-- 1 joshua staff 16384 Mar 22 15:32 file1.txt
    // -rw-r--r-- 1 joshua staff  4096 Mar 22 15:32 file2.txt
    // -rw-r--r-- 1 joshua staff  4096 Mar 22 15:32 file3.txt

    pub fn read_from_disk<P: AsRef<Path>>(
        filepath: P,
        chunk_size: usize,
    ) -> Result<Vec<ChunkMeta>, BluError> {
        let chunker = Chunkerator::new(filepath, chunk_size)?;
        let chunkmetas: Vec<ChunkMeta> = chunker.into_iter().map(|e| ChunkMeta::new(&e)).collect();
        Ok(chunkmetas)
    }

    #[test]
    fn read_blocks() {
        let file1_path = Path::new(TEST_BLOCKS_DIR_T1).join("file1.txt");
        let chunk_metas1 = read_from_disk(file1_path, 4096).unwrap();
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
        let chunk_metas2 = read_from_disk(file2_path, 4096).unwrap();
        assert_eq!(chunk_metas2, vec![
                ChunkMeta {
                    hash: Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                    size: 4096,
                },
            ],
        );

        let file3_path = Path::new(TEST_BLOCKS_DIR_T1).join("file3.txt");
        let chunk_metas3 = read_from_disk(file3_path, 4096).unwrap();
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
                    references: HashMap::from([
                        (
                            Hash::from("1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd"),
                            Position {
                                offset: 0,
                                size: 1024,
                            },
                        ),
                    ]),
                }
            ),
            (
                Hash::from("134089e75f89ca624a073a1b3648303a4abd77fd49325110aa08d683ea0a03de6f949650bbf74f33597f5dcc54c57aaeb47cd143452a320f06c69829c54dc7d9dbb5"),
                BlockRef {
                    references: HashMap::from([
                        (
                            Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                            Position {
                                offset: 4096,
                                size: 4096,
                            },
                        ),
                    ]),
                },
            ),
            (
                Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                BlockRef {
                    references: HashMap::from([
                        (
                            Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                            Position {
                                offset: 0,
                                size: 4096,
                            },
                        ),
                        (
                            Hash::from("1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726"),
                            Position {
                                offset: 0,
                                size: 4096,
                            },
                        ),
                    ]),
                },
            ),
            (
                Hash::from("1340854c0357e05ac2c579e0fac9e2f1be10e6f2e8e678bb0005592a60251d885ceda96764e3b75af33e53e204dc868a036c63354a6a402699e9b613a31a9c5b5549"),
                BlockRef {
                    references: HashMap::from([
                        (
                            Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                            Position {
                                offset: 12288,
                                size: 4096,
                            },
                        ),
                    ]),
                },
            ),
            (
                Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                BlockRef {
                    references: HashMap::from([
                        (
                            Hash::from("13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0"),
                            Position {
                                offset: 8192,
                                size: 4096,
                            },
                        ),
                        (
                            Hash::from("13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829"),
                            Position {
                                offset: 0,
                                size: 4096,
                            },
                        ),
                    ]),
                },
            ),
        ])
    }

    #[test]
    fn indexes() {
        let index = PlainIndex::new_custom_chunk_size(TEST_BLOCKS_DIR_T1, 4096).unwrap();

        assert_eq!(index.files, helper_files_map());
        assert_eq!(index.blocks, helper_blocks_map());
    }
}
