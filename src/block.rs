use crate::hash;
use crate::magic::Wizard;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::{fmt, fs};

const BLOCK_SIZE: usize = 4096;
use crate::chunkfile::ChunkFile;

// File ==> ... hash?
// pub struct FSHandle {
//     File
// }
// FSHandle => {
//     label => PathBuf,
//     file => &File,
// } ... ??? Sth like this?

// TODO: Walk the dir and try it that way ...
// PlainIndex::new(dir)   // walk dir
// pub struct PlainIndex {
//     files_map: HashMap<Vec<u8>, File>
//     labels_map ??? : HashMap<Vec<u8>, HashSet<PathBuf> ??
// }

#[derive(Debug, PartialEq, Clone, Hash, Serialize, Deserialize)]
pub struct Block {
    // hash: Vec<u8>,
    hash: MyHash,
    // data: Vec<u8>,
    size: usize,
}

impl Block {
    pub fn new(data: &[u8]) -> Self {
        let mh = hash::hash(data);
        Self {
            hash: MyHash::from(mh.to_bytes()),
            size: data.len(),
        }
    }

    pub fn hash(&self) -> Vec<u8> {
        self.hash.bytes()
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct File {
    blocks: Vec<Block>,
    // TODO: ref table?
    filetype: String,
}

// == encrypted parts

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct ChunkFileLocation {
    path: PathBuf,
    index: usize,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Default)]
pub struct EncryptedBlockIndex {
    // map the encrypted hash to the location of the data on disk
    map: HashMap<Vec<u8>, ChunkFileLocation>,
}

impl EncryptedBlockIndex {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn add_chunk_location(&mut self, chunk_hash: &[u8], location: &ChunkFileLocation) -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    // returns the encrypted from disk, decrypt it yourself
    //
    // TODO: seems REALLY weird to just open a new ChunkFile on disk every time
    // to read a single block ... should we maintain a map of open files for
    // reading the chunks? e.g. once this particular location is opened, we
    // don't close it, keep it open at least for X most recently accessed
    // files?
    pub fn get_enc_block(&self, hash: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let enc_location = self.map.get(hash).ok_or("location not found")?;

        let mut f = fs::File::open(&enc_location.path)?;
        let mut chunkdata = Vec::new();
        let _bytes_read = f.read(&mut chunkdata)?;
        let chunkfile = ChunkFile::deserialize(&chunkdata)?;

        chunkfile.get_chunk(enc_location.index)
    }
}

// all this to debug the Vec<u8> as a hex string instead of numbers
#[derive(Serialize, Deserialize, PartialEq, Clone, Hash)]
pub struct MyHash(Vec<u8>);
impl std::fmt::Debug for MyHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = write!(f, "{:?}", &hex::encode(&self.0));
        Ok(())
    }
}
impl<'a> From<Vec<u8>> for MyHash {
    fn from(vec: Vec<u8>) -> Self {
        Self(vec)
    }
}
impl MyHash {
    pub fn bytes(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}

impl File {
    //                    Vec<u8> ... not sure about this... debugging Vec<u8>
    //                    sucks
    pub fn hash(&self) -> MyHash {
        let mut all_hashes = vec![];
        for block in self.blocks.iter() {
            let mut block_hash = block.hash();
            all_hashes.append(&mut block_hash);
        }

        MyHash::from(hash::hash(&all_hashes).to_bytes())
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
            if data.len() == 0 {
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
    use super::BLOCK_SIZE;
    use std::fs::{self, File};
    use std::io::Read;
    use std::io::{BufRead, BufReader};
    use std::path::Path;

    const TEST_BLOCKS_DIR_T1: &str = "test/blocks/t1/";
    #[test]
    fn read_blocks() {
        // TODO: duplicate this, but with the same buf every time ...
        // ... and do sth w/block
        //

        // fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize>
        //
        // Read all bytes until EOF in this source, placing them into buf.
        //
        // All bytes read from this source will be appended to the specified
        // buffer buf.
        //
        // This function will continuously call read() to append more data to
        // buf until read() returns either Ok(0) or an error of
        // non-ErrorKind::Interrupted kind.
        //
        // If successful, this function will return the total number of bytes
        // read.
        //
        // Errors
        //
        // If this function encounters an error of the kind
        // ErrorKind::Interrupted then the error is ignored and the operation
        // will continue.
        //
        // If any other read error is encountered then this function immediately
        // returns. Any bytes which have already been read will be appended to
        // buf.

        // see also BufReader
        // https://doc.rust-lang.org/std/io/struct.BufReader.html#method.with_capacity

        let file1_path = Path::new(TEST_BLOCKS_DIR_T1).join("file1.txt");
        let file1 = super::File::read_from_disk(file1_path).unwrap();
        dbg!(&file1);
        dbg!(&file1.hash());

        let file2_path = Path::new(TEST_BLOCKS_DIR_T1).join("file2.txt");
        let file2 = super::File::read_from_disk(file2_path).unwrap();
        dbg!(&file2);
        dbg!(&file2.hash());

        let file3_path = Path::new(TEST_BLOCKS_DIR_T1).join("file3.txt");
        let file3 = super::File::read_from_disk(file3_path).unwrap();
        dbg!(&file3);
        dbg!(&file3.hash());

        // -rw-r--r-- 1 nmarley staff 16384 Mar 22 15:32 file1.txt
        // -rw-r--r-- 1 nmarley staff  4096 Mar 22 15:32 file2.txt
        // -rw-r--r-- 1 nmarley staff  4096 Mar 22 15:32 file3.txt
        assert_eq!(1, 0);
    }
}
