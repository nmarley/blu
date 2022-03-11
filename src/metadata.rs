use flate2::bufread::{GzDecoder, GzEncoder};
use flate2::Compression;
use multihash::{Code, MultihashDigest};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{
    fmt, fs,
    io::{self, Read},
    path::Path,
};
use walkdir::WalkDir;

use crate::config::KeyID;
use crate::magic::Wizard;

// TODO: rename this struct ...
// FileMeta? Archive?
#[derive(PartialEq, Serialize, Deserialize, Clone)]
pub struct Entry {
    // paths: Vec<std::path::Path>,
    paths: Vec<String>,
    filetype: String,

    hash: Vec<u8>,
    size: u64,
    enc: Option<Encrypted>,

    tags: Vec<String>,     // TODO: proper tagging, or... ?
    notes: Option<String>, // free-form text
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("paths", &self.paths)
            .field("filetype", &self.filetype)
            .field("hash", &hex::encode(&self.hash))
            .field("size", &self.size)
            .field("enc", &self.enc)
            .field("tags", &self.tags)
            .field("notes", &self.notes)
            .finish()
    }
}

// TODO: rename ?
#[derive(PartialEq, Serialize, Deserialize, Clone)]
pub struct Encrypted {
    hash: Vec<u8>,
    size: u64,
    keys: Vec<KeyID>,
}

impl fmt::Debug for Encrypted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encrypted")
            .field("hash", &hex::encode(&self.hash))
            .field("size", &self.size)
            .field("keys", &self.keys)
            .finish()
    }
}

fn ser_map(map_files: &HashMap<Vec<u8>, Entry>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let encoded: Vec<u8> = bincode::serialize(map_files)?;
    // let encoded: Vec<u8> = serde_cbor::to_vec(map_files)?;
    Ok(encoded)
}

fn deser_map(data: &[u8]) -> Result<HashMap<Vec<u8>, Entry>, Box<dyn std::error::Error>> {
    let decoded: HashMap<Vec<u8>, Entry> = bincode::deserialize(data)?;
    // let decoded: HashMap<Vec<u8>, Entry> = serde_cbor::from_slice(data)?;
    Ok(decoded)
}

#[allow(dead_code)]
fn compress(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut gz = GzEncoder::new(data, Compression::fast());
    let mut buf = Vec::new();
    gz.read_to_end(&mut buf)?;
    Ok(buf)
}

#[allow(dead_code)]
fn decompress(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut gz = GzDecoder::new(data);
    let mut buf = Vec::new();
    gz.read_to_end(&mut buf)?;
    Ok(buf)
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Index {
    map: HashMap<Vec<u8>, Entry>,
}

impl Index {
    // note: NOT SURE YET if this is the interface I want to offer ...
    pub fn new<P: AsRef<Path>>(dir: P) -> Self {
        // TODO: unwrap, seriously? fix this <<----
        let map = Self::index(dir).unwrap();
        Index { map }
    }

    pub fn deserialize(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        let index = Index {
            map: deser_map(data)?,
        };
        Ok(index)
    }

    pub fn serialize(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let x = ser_map(&self.map)?;
        Ok(x)
    }

    // TODO: remove? currently used in 1 test below
    pub fn get_entry_ref(&self, hash: &[u8]) -> Result<&Entry, Box<dyn std::error::Error>> {
        let e = self.map.get(hash).unwrap();
        Ok(e)
    }

    // walk the dir and hash all regular files
    // ignore block/char specials, etc.
    fn index<P: AsRef<Path>>(
        base_dir: P,
    ) -> Result<HashMap<Vec<u8>, Entry>, Box<dyn std::error::Error>> {
        let mut count = 0usize;

        // TODO: only build a new hashmap if we don't get metadata from the DB already
        let mut map_files = HashMap::new();

        // chdir into base before walking
        //
        // otherwise we get paths like "./test/file.txt" if we set the base dir to
        // "./test"

        // let current_dir = env::current_dir()?;
        // env::set_current_dir(&base_dir)?;

        let wiz = Wizard::new();

        for entry in WalkDir::new(&base_dir).into_iter().filter_map(|e| e.ok()) {
            let bludir = Path::new(base_dir.as_ref().as_os_str()).join(".blu/");
            dbg!(&bludir);
            // skip special .blu dir
            // TODO: fix this shite, normalize path prefixes
            if entry.path().starts_with(bludir) {
                continue;
            }

            // for initial debugging
            if count == 5 {
                break;
            }

            // TODO: allow symlinks?
            if !entry.file_type().is_file() {
                continue;
            }
            count += 1;

            let metadata = fs::metadata(entry.path())?;
            let size = metadata.len();
            println!("{:?}: {:?} bytes", entry.path(), size);

            // TODO: streaming reads here? as some files could be GB in size...
            let filedata = fs::read(entry.path()).unwrap();
            let filetype = wiz
                .get_filetype(&filedata, size)
                .unwrap_or_else(|_| "other".into());
            // dbg!(&filetype);
            let mh = Code::Sha2_512.digest(&filedata);

            // e2 is a reference to the entry in the hashmap ...
            let e2 = map_files.entry(mh.to_bytes()).or_insert(Entry {
                filetype,
                paths: vec![],
                size,
                hash: mh.to_bytes(),
                enc: None,
                tags: vec![],
                notes: None,
            });
            // ... so when it gets modified here, it is updated in the hashmap
            // TODO: fix this, serialize correctly
            e2.paths.push(entry.path().display().to_string());
        }

        // only print entries once
        for e2 in map_files.values() {
            dbg!(&e2);
            println!("========================================================================");
        }

        // now go back to previous state
        // env::set_current_dir(current_dir)?;

        Ok(map_files)
    }
}

#[cfg(test)]
mod test {
    use super::{compress, deser_map, ser_map, Entry, HashMap, Index};
    use multihash::{Code, MultihashDigest};

    const TEST_DIR_T0: &str = "test/t0/";
    // const TEST_DIR_T1: &str = "test/t1/";
    // const TEST_DIR_T2: &str = "test/t2/";

    #[test]
    fn index() {
        let index = Index::new(TEST_DIR_T0);

        // dbg!(&map_files);
        let art1_hash = hex::decode("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6").unwrap();
        let entry = index.get_entry_ref(&art1_hash).unwrap();

        assert_eq!(
            Entry {
                paths: vec![
                    "test/t0/art1_dup_en.txt".to_string(),
                    "test/t0/article1_en.txt".to_string()
                ],
                filetype: "ASCII text".to_string(),
                size: 171,
                hash: art1_hash,
                enc: None,
                tags: vec![],
                notes: None,
            },
            *entry
        );
    }

    fn test_entry(content: &str) -> Entry {
        let b = content.as_bytes();
        let mh = Code::Sha2_512.digest(b);
        Entry {
            paths: vec!["testfile.txt".to_string()],
            filetype: "ASCII text".to_string(),
            size: b.len() as u64,
            hash: mh.to_bytes(),
            enc: None,
            tags: vec![],
            notes: None,
        }
    }

    #[test]
    fn ser_de_map() {
        let entries: Vec<Entry> = vec![test_entry("one"), test_entry("two")];
        let mut map = HashMap::new();
        for e in entries.into_iter() {
            let ehash = e.hash.clone();
            let _ = map.entry(ehash).or_insert(e);
        }

        let serialized_map = ser_map(&map).unwrap();
        println!(
            "{} (len {} bytes)",
            &hex::encode(&serialized_map),
            serialized_map.len()
        );

        let compressed_ser_map = compress(&serialized_map).unwrap();
        println!(
            "compressed: {} (len {} bytes)",
            &hex::encode(&compressed_ser_map),
            compressed_ser_map.len()
        );

        let map2 = deser_map(&serialized_map).unwrap();
        assert_eq!(map, map2);
    }
}
