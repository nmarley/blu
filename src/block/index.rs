use chrono::NaiveDateTime;
use multihash::Multihash;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use std::collections::{HashMap, HashSet};
use std::io::{self, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, metadata};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::{oneshot, Mutex};
use tokio::time::{sleep, Duration};
use walkdir::WalkDir;

use crate::age::BlackBox;
use crate::block::DEFAULT_CHUNK_SIZE;
use crate::compression::{compress, decompress};
use crate::format::datetime_format;
use crate::hash::{Hash, SHA2_512};
use crate::io::{gen_std_bbserde, BlackBoxSerializable, Position};

use super::blockref::BlockRef;
use super::ChunkMeta;
use super::FileRef;

/// the default on-disk filename for the plain index
pub const INDEX_FILENAME: &str = "index.dat";
const CURRENT_INDEX_VERSION: &str = "0.2.1";
/// Number of threads to use for reading files
const NUM_FILE_THREADS: usize = 8;

/// PlainIndex is a struct that represents the index of a directory of files.
///
/// It contains the mapping of file hashes to FileRefs, and block hashes to
/// BlockRefs.
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
    /// Create a new PlainIndex given a directory path.
    pub async fn new<P: AsRef<Path>>(dir: P) -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_custom_chunk_size(dir, DEFAULT_CHUNK_SIZE).await
    }

    /// Create a new PlainIndex given a directory path and custom (non-default)
    /// chunk size.
    pub async fn new_custom_chunk_size<P: AsRef<Path>>(
        dir: P,
        chunk_size: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut idx = Self::new_empty();
        idx.add(dir, Some(chunk_size)).await?;
        Ok(idx)
    }

    /// Add entries to the PlainIndex given a file/dir path and chunk size.
    pub async fn add<P: AsRef<Path>>(
        &mut self,
        path: P,
        chunk_size: Option<usize>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let chunk_size = match chunk_size {
            Some(cs) => cs,
            None => DEFAULT_CHUNK_SIZE,
        };
        // info!("In add, path={:?}", path.as_ref());

        // filter all '.blu' files + dirs
        if path.as_ref().starts_with(".blu/") || path.as_ref().starts_with("./.blu/") {
            return Err("cannot add .blu files or dirs".into());
        }

        match path.as_ref() {
            p if p.is_file() => {
                // add file element
                self.hash_and_add_file(p, chunk_size).await?;
            }
            p if p.is_dir() => {
                // walk dir and add each file element
                for entry in WalkDir::new(p)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| !e.path().starts_with(".blu/"))
                    .filter(|e| !e.path().starts_with("./.blu/"))
                    .filter(|e| e.path().is_file())
                {
                    self.hash_and_add_file(entry.path(), chunk_size).await?;
                }
            }
            p => {
                // skip if non-file and non-dir
                info!("skipping non-file and non-dir {:?}", p);
            }
        }

        Ok(())
    }

    /// Lower-level internal method to hash a file and add to the file and
    /// block indexes.
    async fn hash_and_add_file<P: AsRef<Path>>(
        &mut self,
        pathref: P,
        chunk_size: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // chunking, full file hashing
        let mut chunkmetas: Vec<ChunkMeta> = vec![];

        let file_stat = metadata(pathref.as_ref()).await?;
        let size: usize = file_stat.len() as usize;
        let extra_chunk = if size % chunk_size == 0 { 0 } else { 1 };
        let total_chunks = size / chunk_size + extra_chunk;
        let num_file_threads = std::cmp::min(NUM_FILE_THREADS, total_chunks);

        // TODO: limit to NUM_FILE_THREADS or something for tasks per file
        // Channel for the work Pull from the channel for reading the file When
        // work channel is done, close it. When done reading the file, close
        // the read channel Wait 'til all reads done before finishing and
        // stitching together In theory we could hash the chunks as they come
        // in, but that would require ordering them and waiting 'til each is
        // done.

        // Create an mpmc channel for streaming work into hasher tasks. Bounded
        // in order to handle backpressure. This number is totally arbitrary.
        let (tx, rx) = async_channel::bounded::<(usize, usize)>(num_file_threads * 8);

        // Create a vec to hold the handles for the producer, reader + stitcher tasks
        let mut handles = Vec::with_capacity(num_file_threads + 2);

        // producer
        handles.push(tokio::spawn(async move {
            let mut offset = 0usize;
            while offset < size {
                let curr_chunk_size: usize = std::cmp::min(chunk_size, size - offset);
                tx.send((offset, curr_chunk_size)).await.unwrap();
                offset += curr_chunk_size;
            }
            // drop tx to signal the end of the stream
            drop(tx);
        }));

        let m: Arc<Mutex<HashMap<usize, Vec<u8>>>> = Arc::new(Mutex::new(HashMap::new()));
        let path = pathref.as_ref().to_owned().clone();
        for _ in 0..num_file_threads {
            let path = path.clone();
            let rx = rx.clone();
            let m = m.clone();
            handles.push(tokio::spawn(async move {
                let mut fh = match fs::File::open(&path).await {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("{}", e);
                        panic!("unable to open file");
                    }
                };
                while let Ok((offset, size)) = rx.recv().await {
                    let order = offset / chunk_size;
                    match fh.seek(SeekFrom::Start(offset as u64)).await {
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("{}", e);
                            panic!("unable to seek");
                        }
                    }
                    let mut bytes: Vec<u8> = vec![0u8; size];
                    let _bytes_read = match fh.read_exact(&mut bytes).await {
                        Ok(size) => size,
                        Err(e) => {
                            eprintln!("{}", e);
                            panic!("unable to read bytes from file");
                        }
                    };
                    m.lock().await.insert(order, bytes);
                }
                drop(rx);
            }));
        }
        // drop rx to signal the end of the stream
        drop(rx);

        // Create a oneshot channel for sending the full-file hash and the
        // chunkmetas back to the main task once it's finished
        let (hashes_tx, hashes_rx) = oneshot::channel::<(Hash, Vec<ChunkMeta>)>();

        // synchronize access to the hashmap
        // final hashmap reader -- reorder pieces and stitch them together
        handles.push(tokio::spawn(async move {
            // TODO: extensible hashing -- get hasher type from config / hasher
            // from factory
            let mut hasher = Sha512::new();
            for i in 0..total_chunks {
                // Spin until the chunk is available
                loop {
                    if let Some(bytes) = m.lock().await.remove(&i) {
                        chunkmetas.push(ChunkMeta::new(&bytes));
                        hasher.update(&bytes);
                        break;
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            }
            let file_mh: Multihash<64> = Multihash::wrap(SHA2_512, &hasher.finalize()).unwrap();
            let file_hash = Hash::from(file_mh.to_bytes());
            // Send the full-file hash and chunkmetas back to the main task
            let _ = hashes_tx.send((file_hash, chunkmetas));
        }));

        // Wait for all the tasks to finish
        futures::future::join_all(handles).await;

        // Attempt to receive the full-file hash and chunkmetas from the final
        // hashmap reader (stitcher)
        let (file_hash, chunkmetas) = match hashes_rx.await {
            Ok((file_hash, chunkmetas)) => (file_hash, chunkmetas),
            Err(e) => {
                return Err(Box::new(e));
            }
        };

        // block index
        let mut offset = 0;
        for cm_ref in chunkmetas.iter() {
            let blockref = self.blocks.entry(cm_ref.hash.clone()).or_default();
            blockref.references.insert(
                file_hash.clone(),
                Position {
                    size: cm_ref.size,
                    offset,
                },
            );
            offset += cm_ref.size;
        }

        // normalize paths by removing `./` prefix
        let mut path = pathref.as_ref().to_path_buf();
        if path.starts_with("./") {
            path = path.strip_prefix("./")?.to_path_buf();
        }

        // add path to this hash in file index
        self.files
            .entry(file_hash)
            .or_insert_with(|| FileRef::new(chunkmetas))
            .paths
            .insert(path);

        Ok(())
    }

    /// Create a new, empty PlainIndex.
    pub fn new_empty() -> Self {
        let files: HashMap<Hash, FileRef> = HashMap::new();
        let blocks: HashMap<Hash, BlockRef> = HashMap::new();
        Self {
            files,
            blocks,
            version: CURRENT_INDEX_VERSION.to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    /// Returns the number of unique bytes indexed.
    pub fn uniq_bytes_indexed(&self) -> u64 {
        self.blocks.iter().fold(0u64, |acc, elem| {
            elem.1.references.iter().next().unwrap().1.size as u64 + acc
        })
    }

    /// Returns the total number of bytes indexed.
    pub fn total_bytes_indexed(&self) -> u64 {
        self.blocks.iter().fold(0u64, |acc, elem| {
            acc + elem
                .1
                .references
                .iter()
                .fold(0u64, |inner_acc, inner_elem| {
                    inner_elem.1.size as u64 + inner_acc
                })
        })
    }

    /// Returns the number of duplicate bytes indexed (bytes saved in encryption step by
    /// de-duplicating beforehand).
    pub fn duplicate_bytes_indexed(&self) -> u64 {
        self.total_bytes_indexed() - self.uniq_bytes_indexed()
    }

    /// Returns the number of unique blocks (not files) indexed.
    pub fn count_blocks(&self) -> usize {
        self.blocks.len()
    }

    /// Returns an iterator over the hashes of all files in the index.
    pub fn iter_hashes(&self) -> impl Iterator<Item = &Hash> {
        self.files.keys()
    }

    /// Get a shared reference to the files map.
    pub fn files_map_ref(&self) -> &HashMap<Hash, FileRef> {
        &self.files
    }

    /// Get a shared reference to the blocks map.
    pub fn blocks_map_ref(&self) -> &HashMap<Hash, BlockRef> {
        &self.blocks
    }

    /// Get a reference to the FileRef for a given hash.
    pub fn get_fileref_ref(&self, file_hash: &Hash) -> Option<&FileRef> {
        self.files.get(file_hash)
    }

    // TODO: Should this be block hash instead?
    /// Read the bytes from disk and return them for a given blockref.
    pub async fn read_block_bytes(&self, blockref: &BlockRef) -> Vec<u8> {
        let (file_hash, disk_index) = blockref.references.iter().next().unwrap();
        let fileref = self.get_fileref_ref(file_hash).unwrap();
        let filename = fileref.get_a_path();

        // TODO: don't unwrap
        let mut f = fs::File::open(filename).await.unwrap();
        let mut buf: Vec<u8> = vec![0; disk_index.size];
        let _ = f.seek(SeekFrom::Start(disk_index.offset as u64)).await;
        f.read_exact(&mut buf).await.unwrap();
        buf
    }

    /// Update the existing index, given a directory path, and return a list of removed (dangling)
    /// entries.
    pub async fn update<P: AsRef<Path>>(
        &mut self,
        base_dir: P,
        chunk_size: usize,
    ) -> Result<(Vec<FileRef>, Vec<BlockRef>), Box<dyn std::error::Error>> {
        let new_index = Self::new_custom_chunk_size(base_dir, chunk_size).await?;

        let mut to_delete: HashSet<Hash> = HashSet::new();
        let mut new_paths: HashMap<Hash, HashSet<PathBuf>> = HashMap::new();
        let mut is_updated = false;

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

        // for each hash/fileref in NEW, add it
        for (hash, fileref) in new_index.files.into_iter() {
            self.files.entry(hash).or_insert(fileref);
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
        let mut new_references: HashMap<Hash, HashMap<Hash, Position>> = HashMap::new();

        // for each blockref in OLD ...
        for hash in self.blocks.keys() {
            if let Some(blockref) = new_index.blocks.get(hash) {
                // update the references
                new_references.insert(hash.clone(), blockref.references.clone());
            } else {
                // this blockref should be removed
                // ... add it to to_delete
                to_delete.insert(hash.clone());
            }
        }

        // set new references
        for (hash, references) in new_references.into_iter() {
            self.blocks
                .entry(hash)
                .and_modify(|e| e.references = references);
        }

        // for each hash/blockref in NEW, add it
        for (hash, blockref) in new_index.blocks.into_iter() {
            self.blocks.entry(hash).or_insert(blockref);
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

    /// Traverse filerefs and build a hashmap of <path, file hash>
    pub fn build_path_index(&self) -> HashMap<PathBuf, Hash> {
        let mut map: HashMap<PathBuf, Hash> = HashMap::new();

        for (file_hash, fileref) in &self.files {
            for path in &fileref.paths {
                map.insert(path.clone(), file_hash.clone());
            }
        }

        map
    }
}

gen_std_bbserde!(PlainIndex);

/// Helper method to return the current timestamp
fn now() -> chrono::NaiveDateTime {
    // returns a NaiveDateTime without milli/nano seconds
    NaiveDateTime::from_timestamp(chrono::Utc::now().timestamp(), 0)
}

#[cfg(test)]
mod test {
    use std::collections::{HashMap, HashSet};
    use std::path::Path;
    use tokio::fs;

    use super::{BlockRef, ChunkMeta, FileRef, PlainIndex, Position};
    use crate::hash::Hash;

    const TEST_BLOCKS_DIR_T5: &str = "test/blocks/t5/";

    // 'a' * 4095 + '\n'
    const HASH_A_4095_NEWLINE: &str = "1340518b2b49cb74c652eabb2269d823032c46d9ad431b7996ee842b4e295e8da50c1500070b86919140e5eedf317abe8d5bfb11a8362bcd0c864cb975d1cee1c726";
    // 'b' * 4095 + '\n'
    const HASH_B_4095_NEWLINE: &str = "134089e75f89ca624a073a1b3648303a4abd77fd49325110aa08d683ea0a03de6f949650bbf74f33597f5dcc54c57aaeb47cd143452a320f06c69829c54dc7d9dbb5";
    // 'c' * 4095 + '\n'
    const HASH_C_4095_NEWLINE: &str = "13406145743977536da9120fa85aa5e7a3af3463ed47711450684c32da5992a7ae9de9744b5baf0115b359b8d035f10005402f3bf809d10c6aedbdc2942e0ff6c829";
    // 'd' * 4095 + '\n'
    const HASH_D_4095_NEWLINE: &str = "1340854c0357e05ac2c579e0fac9e2f1be10e6f2e8e678bb0005592a60251d885ceda96764e3b75af33e53e204dc868a036c63354a6a402699e9b613a31a9c5b5549";
    // 'e' * 4095 + '\n'
    const HASH_E_4095_NEWLINE: &str = "1340a2186f7619d9b6cf298d9cf1d3a2fb02f916b275b749280c490f701bbf4efecd8f4dd0fb8ba9d806bcf7a26419166601e77bc8f25314e38fc336e55d8dc25de8";
    // 'f' * 4095 + '\n'
    const HASH_F_4095_NEWLINE: &str = "13401b9b1047eed22db2f29b3d93838a9d6d0aea552f4a8284bb554fe1fb98c6b71b53a9917020472b50421235cd9e65e43c54e052abd16c18fd867347b0b7eae0ae";

    // 'a' * 1023 + '\n'
    const HASH_A_1023_NEWLINE: &str = "1340e41807487745dceea0d9f154d8470519ba3ea9e94b1524afd3e4ace63e66ad803d1504b6f2cccc33fb3fe7d981b0eaef30a7010f2a2a1df12c40e9f1cc67e9dd";

    // 'a' * 4095 + '\n' + ...
    // 'b' * 4095 + '\n' + ...
    // 'c' * 4095 + '\n' + ...
    // 'd' * 4095 + '\n'
    const HASH_FILE1_ABCD4096_16384: &str = "13407055ad6a09e40a17ede4d01b91d3fdb9b598f6a0c6543f5089cae5165ed8a2be38a8cbeb583e0982871431163317073742842518a987c0b35a7c9b3dfe44b9d0";

    // echo 'Hello, there!' | sha512sum
    const HASH_HELLO: &str = "1340d58359f9a20ea1864c246ed06797907f3fc9cdc4b50099b2c943beb18bbc4e07650de9056b4491dfdd94dc47801e30db12344735aa06cefdb6d09c49fb75e25c";

    // - ensures that pathbufs are updated
    // - ensures that deleted filerefs and blockrefs are removed from index
    // - ensures that added filerefs and blockrefs are added to index
    #[tokio::test]
    async fn update_index() {
        let chunk_size = 4096;
        let mut index = PlainIndex::new_custom_chunk_size(TEST_BLOCKS_DIR_T5, chunk_size)
            .await
            .unwrap();

        let before_filerefs = HashMap::from([
            (
                // file_f
                Hash::from(HASH_F_4095_NEWLINE),
                FileRef {
                    chunkmetas: vec![ChunkMeta {
                        hash: Hash::from(HASH_F_4095_NEWLINE),
                        size: 4096,
                    }],
                    paths: HashSet::from(["test/blocks/t5/file_f.txt".into()]),
                },
            ),
            (
                // file5
                Hash::from(HASH_A_1023_NEWLINE),
                FileRef {
                    chunkmetas: vec![ChunkMeta {
                        // file5
                        hash: Hash::from(HASH_A_1023_NEWLINE),
                        size: 1024,
                    }],
                    paths: HashSet::from(["test/blocks/t5/file5.txt".into()]),
                },
            ),
            (
                // file1
                Hash::from(HASH_FILE1_ABCD4096_16384),
                FileRef {
                    chunkmetas: vec![
                        ChunkMeta {
                            hash: Hash::from(HASH_A_4095_NEWLINE),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from(HASH_B_4095_NEWLINE),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from(HASH_C_4095_NEWLINE),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from(HASH_D_4095_NEWLINE),
                            size: 4096,
                        },
                    ],
                    paths: HashSet::from(["test/blocks/t5/file1.txt".into()]),
                },
            ),
            (
                Hash::from(HASH_C_4095_NEWLINE),
                FileRef {
                    chunkmetas: vec![ChunkMeta {
                        hash: Hash::from(HASH_C_4095_NEWLINE),
                        size: 4096,
                    }],
                    paths: HashSet::from(["test/blocks/t5/file4.txt".into()]),
                },
            ),
            (
                Hash::from(HASH_A_4095_NEWLINE),
                FileRef {
                    chunkmetas: vec![ChunkMeta {
                        hash: Hash::from(HASH_A_4095_NEWLINE),
                        size: 4096,
                    }],
                    paths: HashSet::from([
                        "test/blocks/t5/file2.txt".into(),
                        "test/blocks/t5/file3.txt".into(),
                    ]),
                },
            ),
        ]);

        let before_blockrefs = HashMap::from([
            (
                Hash::from(HASH_F_4095_NEWLINE),
                BlockRef {
                    references: HashMap::from([(
                        // file_f
                        Hash::from(HASH_F_4095_NEWLINE),
                        Position {
                            offset: 0,
                            size: 4096,
                        },
                    )]),
                },
            ),
            (
                // file5
                Hash::from(HASH_A_1023_NEWLINE),
                BlockRef {
                    references: HashMap::from([(
                        // file5
                        Hash::from(HASH_A_1023_NEWLINE),
                        Position {
                            offset: 0,
                            size: 1024,
                        },
                    )]),
                },
            ),
            (
                Hash::from(HASH_B_4095_NEWLINE),
                BlockRef {
                    references: HashMap::from([(
                        // file1
                        Hash::from(HASH_FILE1_ABCD4096_16384),
                        Position {
                            offset: 4096,
                            size: 4096,
                        },
                    )]),
                },
            ),
            (
                Hash::from(HASH_A_4095_NEWLINE),
                BlockRef {
                    references: HashMap::from([
                        (
                            // file 1
                            Hash::from(HASH_FILE1_ABCD4096_16384),
                            Position {
                                offset: 0,
                                size: 4096,
                            },
                        ),
                        (
                            // file 2 + file 3
                            Hash::from(HASH_A_4095_NEWLINE),
                            Position {
                                offset: 0,
                                size: 4096,
                            },
                        ),
                    ]),
                },
            ),
            (
                Hash::from(HASH_D_4095_NEWLINE),
                BlockRef {
                    references: HashMap::from([(
                        Hash::from(HASH_FILE1_ABCD4096_16384),
                        Position {
                            // file1
                            offset: 12288,
                            size: 4096,
                        },
                    )]),
                },
            ),
            (
                Hash::from(HASH_C_4095_NEWLINE),
                BlockRef {
                    references: HashMap::from([
                        (
                            // file 1
                            Hash::from(HASH_FILE1_ABCD4096_16384),
                            Position {
                                offset: 8192,
                                size: 4096,
                            },
                        ),
                        (
                            // file 4
                            Hash::from(HASH_C_4095_NEWLINE),
                            Position {
                                offset: 0,
                                size: 4096,
                            },
                        ),
                    ]),
                },
            ),
        ]);

        assert_eq!(index.files, before_filerefs);
        assert_eq!(index.blocks, before_blockrefs);

        // rename file5.txt to file6.txt
        let dir_path = Path::new(TEST_BLOCKS_DIR_T5);
        fs::rename(dir_path.join("file5.txt"), dir_path.join("file6.txt"))
            .await
            .unwrap();

        // remove file4.txt
        let file4_buf = fs::read(dir_path.join("file4.txt")).await.unwrap();
        fs::remove_file(dir_path.join("file4.txt")).await.unwrap();

        // change content of file3.txt
        let file3_buf = fs::read(dir_path.join("file3.txt")).await.unwrap();
        let mut e_buf = vec![b'e'; 4095];
        e_buf.push(b'\n');
        fs::write(dir_path.join("file3.txt"), e_buf).await.unwrap();

        // remove file_f.txt
        let file_f_buf = fs::read(dir_path.join("file_f.txt")).await.unwrap();
        fs::remove_file(dir_path.join("file_f.txt")).await.unwrap();

        let after_filerefs = HashMap::from([
            (
                Hash::from(HASH_A_1023_NEWLINE),
                FileRef {
                    chunkmetas: vec![ChunkMeta {
                        hash: Hash::from(HASH_A_1023_NEWLINE),
                        size: 1024,
                    }],
                    paths: HashSet::from(["test/blocks/t5/file6.txt".into()]),
                },
            ),
            (
                Hash::from(HASH_FILE1_ABCD4096_16384),
                FileRef {
                    chunkmetas: vec![
                        ChunkMeta {
                            hash: Hash::from(HASH_A_4095_NEWLINE),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from(HASH_B_4095_NEWLINE),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from(HASH_C_4095_NEWLINE),
                            size: 4096,
                        },
                        ChunkMeta {
                            hash: Hash::from(HASH_D_4095_NEWLINE),
                            size: 4096,
                        },
                    ],
                    paths: HashSet::from(["test/blocks/t5/file1.txt".into()]),
                },
            ),
            (
                Hash::from(HASH_A_4095_NEWLINE),
                FileRef {
                    chunkmetas: vec![ChunkMeta {
                        hash: Hash::from(HASH_A_4095_NEWLINE),
                        size: 4096,
                    }],
                    paths: HashSet::from(["test/blocks/t5/file2.txt".into()]),
                },
            ),
            (
                // new file3.txt
                Hash::from(HASH_E_4095_NEWLINE),
                FileRef {
                    chunkmetas: vec![ChunkMeta {
                        hash: Hash::from(HASH_E_4095_NEWLINE),
                        size: 4096,
                    }],
                    paths: HashSet::from(["test/blocks/t5/file3.txt".into()]),
                },
            ),
        ]);

        let after_blockrefs = HashMap::from([
            (
                Hash::from(HASH_A_1023_NEWLINE),
                BlockRef {
                    references: HashMap::from([(
                        // file6
                        Hash::from(HASH_A_1023_NEWLINE),
                        Position {
                            offset: 0,
                            size: 1024,
                        },
                    )]),
                },
            ),
            (
                Hash::from(HASH_B_4095_NEWLINE),
                BlockRef {
                    references: HashMap::from([(
                        // file1
                        Hash::from(HASH_FILE1_ABCD4096_16384),
                        Position {
                            offset: 4096,
                            size: 4096,
                        },
                    )]),
                },
            ),
            (
                Hash::from(HASH_A_4095_NEWLINE),
                BlockRef {
                    references: HashMap::from([
                        (
                            // file1
                            Hash::from(HASH_FILE1_ABCD4096_16384),
                            Position {
                                offset: 0,
                                size: 4096,
                            },
                        ),
                        (
                            // file2
                            Hash::from(HASH_A_4095_NEWLINE),
                            Position {
                                offset: 0,
                                size: 4096,
                            },
                        ),
                    ]),
                },
            ),
            (
                Hash::from(HASH_D_4095_NEWLINE),
                BlockRef {
                    references: HashMap::from([(
                        // file1
                        Hash::from(HASH_FILE1_ABCD4096_16384),
                        Position {
                            offset: 12288,
                            size: 4096,
                        },
                    )]),
                },
            ),
            (
                Hash::from(HASH_C_4095_NEWLINE),
                BlockRef {
                    references: HashMap::from([(
                        // file1
                        Hash::from(HASH_FILE1_ABCD4096_16384),
                        Position {
                            offset: 8192,
                            size: 4096,
                        },
                    )]),
                },
            ),
            (
                Hash::from(HASH_E_4095_NEWLINE),
                BlockRef {
                    references: HashMap::from([(
                        // file3 - new
                        Hash::from(HASH_E_4095_NEWLINE),
                        Position {
                            offset: 0,
                            size: 4096,
                        },
                    )]),
                },
            ),
        ]);

        let (mut filerefs, blockrefs) = index.update(TEST_BLOCKS_DIR_T5, chunk_size).await.unwrap();
        // rename file6.txt back to file5.txt
        fs::rename(dir_path.join("file6.txt"), dir_path.join("file5.txt"))
            .await
            .unwrap();
        // restore file4.txt
        fs::write(dir_path.join("file4.txt"), file4_buf)
            .await
            .unwrap();
        // restore file3.txt
        fs::write(dir_path.join("file3.txt"), file3_buf)
            .await
            .unwrap();
        // restore file_f.txt
        fs::write(dir_path.join("file_f.txt"), file_f_buf)
            .await
            .unwrap();
        // NOTE: DO NOT put any tests between the index.update() call and the
        // restore of the files above ^, otherwise broken tests will mess up the
        // test data.  Not a huge deal since it's in git, but easier this way.

        assert_eq!(index.files, after_filerefs);
        assert_eq!(index.blocks, after_blockrefs);

        let mut deleted_filerefs = Vec::from([
            FileRef {
                chunkmetas: vec![ChunkMeta {
                    hash: Hash::from(HASH_C_4095_NEWLINE),
                    size: 4096,
                }],
                paths: HashSet::from(["test/blocks/t5/file4.txt".into()]),
            },
            FileRef {
                chunkmetas: vec![ChunkMeta {
                    hash: Hash::from(HASH_F_4095_NEWLINE),
                    size: 4096,
                }],
                paths: HashSet::from(["test/blocks/t5/file_f.txt".into()]),
            },
        ]);
        let deleted_blockrefs: Vec<BlockRef> = Vec::from([BlockRef {
            references: HashMap::from([(
                Hash::from(HASH_F_4095_NEWLINE),
                Position {
                    offset: 0,
                    size: 4096,
                },
            )]),
        }]);

        // sort for the comparison below
        filerefs.sort_unstable();
        deleted_filerefs.sort_unstable();

        assert_eq!(deleted_filerefs, filerefs);
        assert_eq!(deleted_blockrefs, blockrefs);
    }

    // TODO: this is tested above, so can probably remove this test and reserve
    // t6 for something else.
    const TEST_BLOCKS_DIR_T6: &str = "test/blocks/t6/";
    #[tokio::test]
    async fn update_index_paths() {
        let chunk_size = 4096;
        let mut index = PlainIndex::new_custom_chunk_size(TEST_BLOCKS_DIR_T6, chunk_size)
            .await
            .unwrap();

        let before_filerefs = HashMap::from([(
            Hash::from(HASH_HELLO),
            FileRef {
                chunkmetas: vec![ChunkMeta {
                    hash: Hash::from(HASH_HELLO),
                    size: 14,
                }],
                paths: HashSet::from(["test/blocks/t6/hi.txt".into()]),
            },
        )]);
        let before_blockrefs = HashMap::from([(
            Hash::from(HASH_HELLO),
            BlockRef {
                references: HashMap::from([(
                    Hash::from(HASH_HELLO),
                    Position {
                        offset: 0,
                        size: 14,
                    },
                )]),
            },
        )]);

        let after_filerefs = HashMap::from([(
            Hash::from(HASH_HELLO),
            FileRef {
                chunkmetas: vec![ChunkMeta {
                    hash: Hash::from(HASH_HELLO),
                    size: 14,
                }],
                paths: HashSet::from(["test/blocks/t6/hello.txt".into()]),
            },
        )]);
        let after_blockrefs = HashMap::from([(
            Hash::from(HASH_HELLO),
            BlockRef {
                references: HashMap::from([(
                    Hash::from(HASH_HELLO),
                    Position {
                        offset: 0,
                        size: 14,
                    },
                )]),
            },
        )]);

        assert_eq!(index.files, before_filerefs);
        assert_eq!(index.blocks, before_blockrefs);

        let old_filename = Path::new(TEST_BLOCKS_DIR_T6).join("hi.txt");
        let new_filename = Path::new(TEST_BLOCKS_DIR_T6).join("hello.txt");
        // rename to test
        fs::rename(&old_filename, &new_filename).await.unwrap();
        // run the update
        let (filerefs, blockrefs) = index.update(TEST_BLOCKS_DIR_T6, chunk_size).await.unwrap();
        // move it back
        fs::rename(&new_filename, &old_filename).await.unwrap();

        assert_eq!(index.files, after_filerefs);
        assert_eq!(index.blocks, after_blockrefs);

        assert_eq!(filerefs, []);
        assert_eq!(blockrefs, []);
    }
}
