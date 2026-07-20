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
        assert_eq!(
            chunk_metas1,
            vec![
                ChunkMeta {
                    hash: Hash::from(
                        "1e2096327aafb1bea0248a1c5f68b02750f868fcf92e3b2255931f3de99703188354"
                    ),
                    size: 4096,
                },
                ChunkMeta {
                    hash: Hash::from(
                        "1e206106a79494135bbf061a6e13606ae548d8d4bf62b315e115ddb3f3fac5f97f88"
                    ),
                    size: 4096,
                },
                ChunkMeta {
                    hash: Hash::from(
                        "1e20749ec9c777ebae8b48e8ee2ecc795d0804cecbbcf97ecd8197841946e7a55eba"
                    ),
                    size: 4096,
                },
                ChunkMeta {
                    hash: Hash::from(
                        "1e208bf6ac4335cff17909161e20b6762031de37802d8e0e6f380b87e2b001ad9f7d"
                    ),
                    size: 4096,
                },
            ]
        );

        let file2_path = Path::new(TEST_BLOCKS_DIR_T1).join("file2.txt");
        let chunk_metas2 = read_from_disk(file2_path, 4096).unwrap();
        assert_eq!(
            chunk_metas2,
            vec![ChunkMeta {
                hash: Hash::from(
                    "1e2096327aafb1bea0248a1c5f68b02750f868fcf92e3b2255931f3de99703188354"
                ),
                size: 4096,
            },],
        );

        let file3_path = Path::new(TEST_BLOCKS_DIR_T1).join("file3.txt");
        let chunk_metas3 = read_from_disk(file3_path, 4096).unwrap();
        // should be equal super::File objects
        assert_eq!(chunk_metas2, chunk_metas3);
    }

    fn helper_files_map() -> HashMap<Hash, FileRef> {
        HashMap::from([
            (
                Hash::from("1e206e65b63b80ff0206f36149096359cb3fb337bc215de82710c5c117f43afcfa39"),
                FileRef {
                    chunkmetas: vec![
                        ChunkMeta {
                            hash: Hash::from("1e206e65b63b80ff0206f36149096359cb3fb337bc215de82710c5c117f43afcfa39"),
                            size: 1024,
                        },
                    ],
                    paths: HashSet::from(["test/blocks/t1/file5.txt".into()])
                },
            ),
            (
                Hash::from("1e20ba3a13d579f962c37f18a8f51080b0e768cb6459934e0f3f279e9f18ab86a887"),
                FileRef {
                    chunkmetas: vec![
                        ChunkMeta {
                            hash: Hash::from("1e2096327aafb1bea0248a1c5f68b02750f868fcf92e3b2255931f3de99703188354"),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from("1e206106a79494135bbf061a6e13606ae548d8d4bf62b315e115ddb3f3fac5f97f88"),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from("1e20749ec9c777ebae8b48e8ee2ecc795d0804cecbbcf97ecd8197841946e7a55eba"),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from("1e208bf6ac4335cff17909161e20b6762031de37802d8e0e6f380b87e2b001ad9f7d"),
                            size: 4096,
                        },
                    ],
                    paths: HashSet::from(["test/blocks/t1/file1.txt".into()])
                },
            ),
            (
                Hash::from("1e20749ec9c777ebae8b48e8ee2ecc795d0804cecbbcf97ecd8197841946e7a55eba"),
                FileRef {
                        chunkmetas: vec![
                            ChunkMeta {
                               hash: Hash::from("1e20749ec9c777ebae8b48e8ee2ecc795d0804cecbbcf97ecd8197841946e7a55eba"),
                               size: 4096,
                            },
                        ],
                    paths: HashSet::from(["test/blocks/t1/file4.txt".into()])
                },
            ),
            (
                Hash::from("1e2096327aafb1bea0248a1c5f68b02750f868fcf92e3b2255931f3de99703188354"),
                FileRef {
                        chunkmetas: vec![
                            ChunkMeta {
                               hash: Hash::from("1e2096327aafb1bea0248a1c5f68b02750f868fcf92e3b2255931f3de99703188354"),
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
                Hash::from("1e206e65b63b80ff0206f36149096359cb3fb337bc215de82710c5c117f43afcfa39"),
                BlockRef {
                    references: HashMap::from([
                        (
                            Hash::from("1e206e65b63b80ff0206f36149096359cb3fb337bc215de82710c5c117f43afcfa39"),
                            Position {
                                offset: 0,
                                size: 1024,
                            },
                        ),
                    ]),
                }
            ),
            (
                Hash::from("1e206106a79494135bbf061a6e13606ae548d8d4bf62b315e115ddb3f3fac5f97f88"),
                BlockRef {
                    references: HashMap::from([
                        (
                            Hash::from("1e20ba3a13d579f962c37f18a8f51080b0e768cb6459934e0f3f279e9f18ab86a887"),
                            Position {
                                offset: 4096,
                                size: 4096,
                            },
                        ),
                    ]),
                },
            ),
            (
                Hash::from("1e2096327aafb1bea0248a1c5f68b02750f868fcf92e3b2255931f3de99703188354"),
                BlockRef {
                    references: HashMap::from([
                        (
                            Hash::from("1e20ba3a13d579f962c37f18a8f51080b0e768cb6459934e0f3f279e9f18ab86a887"),
                            Position {
                                offset: 0,
                                size: 4096,
                            },
                        ),
                        (
                            Hash::from("1e2096327aafb1bea0248a1c5f68b02750f868fcf92e3b2255931f3de99703188354"),
                            Position {
                                offset: 0,
                                size: 4096,
                            },
                        ),
                    ]),
                },
            ),
            (
                Hash::from("1e208bf6ac4335cff17909161e20b6762031de37802d8e0e6f380b87e2b001ad9f7d"),
                BlockRef {
                    references: HashMap::from([
                        (
                            Hash::from("1e20ba3a13d579f962c37f18a8f51080b0e768cb6459934e0f3f279e9f18ab86a887"),
                            Position {
                                offset: 12288,
                                size: 4096,
                            },
                        ),
                    ]),
                },
            ),
            (
                Hash::from("1e20749ec9c777ebae8b48e8ee2ecc795d0804cecbbcf97ecd8197841946e7a55eba"),
                BlockRef {
                    references: HashMap::from([
                        (
                            Hash::from("1e20ba3a13d579f962c37f18a8f51080b0e768cb6459934e0f3f279e9f18ab86a887"),
                            Position {
                                offset: 8192,
                                size: 4096,
                            },
                        ),
                        (
                            Hash::from("1e20749ec9c777ebae8b48e8ee2ecc795d0804cecbbcf97ecd8197841946e7a55eba"),
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
