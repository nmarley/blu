use std::fs;
use std::io::Read;
use std::path::Path;
// use serde::{Deserialize, Serialize};
const BLOCK_SIZE: usize = 4096;

// #[derive(PartialEq, Serialize, Deserialize, Clone, Hash)]
#[derive(PartialEq, Clone, Hash)]
pub struct Block {
    hash: Vec<u8>,
    // data: Vec<u8>,
    size: usize,
}

impl Block {
    // let mh = hash::hash(&filedata);
    // pub fn new() -> Self {
    //     Self {
    //     }
    // }
}

type BlockVec<'a> = Vec<&'a Block>;

// #[derive(PartialEq, Serialize, Deserialize, Clone)]
#[derive(PartialEq, Clone)]
pub struct File<'a> {
    blocks: BlockVec<'a>,
    ref_count: usize,
    // TODO: ref table?
    filetype: String,
}

impl File<'_> {
    pub fn hash(&self) -> Vec<u8> {
        // concatenate all block hashes and ... hash?
        vec![]
    }

    pub fn read_from_disk<P: AsRef<Path> + std::fmt::Debug>(
        path: P,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            // read_blocks()
            // TODO: blocks reading iterator
            // first block can be passed to magic to determine filetype
            blocks: BlockVec::new(),
            ref_count: 0,
            filetype: "".to_string(),
        })
    }
}

// pub struct BlockReader {
// }
// impl Iterator for BlockReader {
//     type Item = Block;
// }

fn read_blocks<R: AsRef<dyn std::io::BufRead>>(reader: R) -> Vec<Vec<u8>> {
    let mut blocks: Vec<Vec<u8>> = vec![];

    let mut size = 1;
    while size > 0 {
        let data = reader.as_ref().fill_buf().unwrap();
        size = data.len();
        reader.as_ref().consume(size);
        blocks.push(data.to_vec());
    }

    blocks
    // let mut reader = BufReader::with_capacity(BLOCK_SIZE, file);
    // dbg!(&BLOCK_SIZE);
    // let block_bytes = reader.fill_buf().unwrap();
    // dbg!(&block_bytes.len());
}

// let file1_path = Path::new(TEST_BLOCKS_DIR_T1).join("file5.txt");
// let file = File::open(file1_path).unwrap();
// let mut reader = BufReader::with_capacity(BLOCK_SIZE, file);
// dbg!(&BLOCK_SIZE);
// let block_bytes = reader.fill_buf().unwrap();
// dbg!(&block_bytes.len());

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

        let file1_path = Path::new(TEST_BLOCKS_DIR_T1).join("file5.txt");
        let file = File::open(file1_path).unwrap();
        let mut reader = BufReader::with_capacity(BLOCK_SIZE, file);
        dbg!(&BLOCK_SIZE);

        let block_bytes = reader.fill_buf().unwrap();
        dbg!(&block_bytes.len());

        // dbg!(&block_bytes);

        // let mut buf: [u8; BLOCK_SIZE] = [0; BLOCK_SIZE];
        // let filedata = file.read(&mut buf);
        // println!("fd: {:?}", filedata);
        // println!("buf: {:?}", buf);
        // dbg!(&buf.len());

        // -rw-r--r-- 1 nmarley staff 16384 Mar 22 15:32 file1.txt
        // -rw-r--r-- 1 nmarley staff  4096 Mar 22 15:32 file2.txt
        // -rw-r--r-- 1 nmarley staff  4096 Mar 22 15:32 file3.txt
        assert_eq!(1, 0);
    }
}
