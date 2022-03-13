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

use crate::age::BlackBox;
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

fn serialize_index(index: &Index) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let encoded: Vec<u8> = bincode::serialize(index)?;
    // let encoded: Vec<u8> = serde_cbor::to_vec(index)?;
    Ok(encoded)
}

fn deserialize_index(data: &[u8]) -> Result<Index, Box<dyn std::error::Error>> {
    let decoded: Index = bincode::deserialize(data)?;
    // let decoded: Index = serde_cbor::from_slice(data)?;
    Ok(decoded)
}

fn compress(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut gz = GzEncoder::new(data, Compression::fast());
    let mut buf = Vec::new();
    gz.read_to_end(&mut buf)?;
    Ok(buf)
}

fn decompress(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut gz = GzDecoder::new(data);
    let mut buf = Vec::new();
    gz.read_to_end(&mut buf)?;
    Ok(buf)
}

#[derive(PartialEq, Serialize, Deserialize)]
pub struct Index {
    map: HashMap<Vec<u8>, Entry>,
    version: String,
}

const CURRENT_INDEX_VERSION: &str = "0.1.0";
impl Index {
    pub fn new<P: AsRef<Path>>(dir: P) -> Result<Self, Box<dyn std::error::Error>> {
        let map = Self::build_index(dir)?;
        Ok(Index {
            version: CURRENT_INDEX_VERSION.to_string(),
            map,
        })
    }

    fn deserialize(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        deserialize_index(data)
    }

    fn serialize(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        serialize_index(self)
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

    // TODO: remove? currently used in 1 test below
    pub fn get_entry_ref(&self, hash: &[u8]) -> Result<&Entry, Box<dyn std::error::Error>> {
        let e = self.map.get(hash).unwrap();
        Ok(e)
    }

    // walk the dir and hash all regular files
    // ignore block/char specials, etc.
    fn build_index<P: AsRef<Path>>(
        base_dir: P,
    ) -> Result<HashMap<Vec<u8>, Entry>, Box<dyn std::error::Error>> {
        let mut count = 0usize;

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
            // skip special .blu dir
            // TODO: normalize path prefixes
            if entry.path().starts_with(bludir) {
                continue;
            }

            // TODO: allow symlinks?
            if !entry.file_type().is_file() {
                continue;
            }
            count += 1;

            let metadata = fs::metadata(entry.path())?;
            let size = metadata.len();
            // println!("{:?}: {:?} bytes", entry.path(), size);

            // TODO: streaming reads here? as some files could be GB in size...
            let filedata = fs::read(entry.path()).unwrap();
            let filetype = wiz
                .get_filetype(&filedata, size)
                .unwrap_or_else(|_| "other".into());
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
            // TODO: fix this, properly serialize paths
            e2.paths.push(entry.path().display().to_string());
        }

        // only print entries once
        // for e2 in map_files.values() {
        //     dbg!(&e2);
        //     println!("========================================================================");
        // }

        // now go back to previous state
        // env::set_current_dir(current_dir)?;

        Ok(map_files)
    }
}

impl fmt::Debug for Index {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = writeln!(f, "Index {{ version: {}, map: ", &self.version);
        for (k, v) in self.map.iter() {
            let _ = write!(f, "\n{}:\n{:?},\n", &hex::encode(k), v);
        }
        let _ = write!(f, "}}");
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::{compress, deserialize_index, serialize_index, Entry, HashMap, Index};
    use multihash::{Code, MultihashDigest};

    const TEST_DIR_T0: &str = "test/t0/";
    // const TEST_DIR_T1: &str = "test/t1/";
    // const TEST_DIR_T2: &str = "test/t2/";

    #[test]
    fn index() {
        let index = Index::new(TEST_DIR_T0).unwrap();
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
    fn ser_de_index() {
        let entries: Vec<Entry> = vec![test_entry("one"), test_entry("two")];
        let mut map = HashMap::new();
        for e in entries.into_iter() {
            let ehash = e.hash.clone();
            let _ = map.entry(ehash).or_insert(e);
        }

        let index = Index {
            version: super::CURRENT_INDEX_VERSION.to_string(),
            map,
        };
        let serialized_idx = serialize_index(&index).unwrap();
        println!(
            "{} (len {} bytes)",
            &hex::encode(&serialized_idx),
            serialized_idx.len()
        );

        let compressed_ser_idx = compress(&serialized_idx).unwrap();
        println!(
            "compressed: {} (len {} bytes)",
            &hex::encode(&compressed_ser_idx),
            compressed_ser_idx.len()
        );

        let idx2 = deserialize_index(&serialized_idx).unwrap();
        assert_eq!(index, idx2);
    }
}
