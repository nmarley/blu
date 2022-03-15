use flate2::bufread::{GzDecoder, GzEncoder};
use flate2::Compression;
use multihash::{Code, MultihashDigest};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::{
    fmt, fs,
    io::{self, Read},
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

use crate::age::BlackBox;
use crate::config::KeyID;
use crate::magic::Wizard;

pub const INDEX_FILENAME: &str = "index.dat";

#[derive(PartialEq, Serialize, Deserialize, Clone)]
pub struct Entry {
    paths: HashSet<PathBuf>,
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

impl Entry {
    pub fn get_enc(&mut self) -> Option<Encrypted> {
        self.enc.clone()
    }

    pub fn read_filedata(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let path = self.paths.iter().nth(0).unwrap();
        let data = fs::read(path)?;
        Ok(data)
    }

    pub fn set_encrypted(&mut self, enc: Encrypted) -> Result<(), Box<dyn std::error::Error>> {
        self.enc = Some(enc);
        Ok(())
    }
}

#[derive(PartialEq, Serialize, Deserialize, Clone)]
pub struct Encrypted {
    // in theory, there won't be multiple files in the encrypted datadir with
    // the same hash
    path: PathBuf,
    hash: Vec<u8>,
    size: u64,
    keys: Vec<KeyID>,
}

impl fmt::Debug for Encrypted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encrypted")
            .field("path", &self.path)
            .field("hash", &hex::encode(&self.hash))
            .field("size", &self.size)
            .field("keys", &self.keys)
            .finish()
    }
}

impl Encrypted {
    pub fn get_hash(&self) -> Vec<u8> {
        self.hash.clone()
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

    pub fn get_entry_ref(&self, hash: &[u8]) -> Result<&Entry, Box<dyn std::error::Error>> {
        let e = self.map.get(hash).unwrap();
        Ok(e)
    }

    // walk the dir and hash all regular files
    // ignore block/char specials, etc.
    fn build_index<P: AsRef<Path>>(
        base_dir: P,
    ) -> Result<HashMap<Vec<u8>, Entry>, Box<dyn std::error::Error>> {
        let mut map_files = HashMap::new();

        // chdir into base before walking
        //
        // otherwise we get paths like "./test/file.txt" if we set the base dir to
        // "./test"

        // let current_dir = env::current_dir()?;
        // env::set_current_dir(&base_dir)?;

        let wiz = Wizard::new();

        for elem in WalkDir::new(&base_dir).into_iter().filter_map(|e| e.ok()) {
            let bludir = Path::new(base_dir.as_ref().as_os_str()).join(".blu/");
            // skip special .blu dir
            // TODO: normalize path prefixes
            if elem.path().starts_with(bludir) {
                continue;
            }

            // TODO: allow symlinks?
            if !elem.file_type().is_file() {
                continue;
            }

            let metadata = fs::metadata(elem.path())?;
            let size = metadata.len();
            // println!("{:?}: {:?} bytes", elem.path(), size);

            // TODO: streaming reads here? as some files could be GB in size...
            let filedata = fs::read(elem.path()).unwrap();
            let filetype = wiz
                .get_filetype(&filedata, size)
                .unwrap_or_else(|_| "other".into());
            let mh = Code::Sha2_512.digest(&filedata);

            // entry is a reference to the entry in the hashmap ...
            let entry = map_files.entry(mh.to_bytes()).or_insert(Entry {
                filetype,
                paths: HashSet::new(),
                size,
                hash: mh.to_bytes(),
                enc: None,
                tags: vec![],
                notes: None,
            });
            // ... so when it gets modified here, it is updated in the hashmap
            entry.paths.insert(elem.into_path());
        }

        // now go back to previous state
        // env::set_current_dir(current_dir)?;

        Ok(map_files)
    }

    // get all entries in the index
    pub fn get_all_entry_refs(&self) -> Vec<&Entry> {
        self.map.values().collect::<Vec<&Entry>>()
    }

    // Return a Vec of Entries that exist in this Index, but do *not* yet exist
    // in the EncIdx.
    //
    // TODO: write tests for this (incl. a tX dir w/some enc files and some not,
    // to make sure this returns the right values)
    pub fn difference_enc_idx<'a, 'b>(&'a self, enc_idx: &'b EncryptedIndex) -> Vec<&'a Entry> {
        let mut to_encrypt: Vec<&Entry> = vec![];
        for entry in self.map.values() {
            match &entry.enc {
                None => to_encrypt.push(entry),
                Some(enc) => {
                    if enc_idx.get_entry_ref(&enc.hash).is_err() {
                        to_encrypt.push(entry);
                    }
                }
            };
        }
        to_encrypt
    }

    // Update the index, return a list of removed (dangling) entries
    pub fn update<P: AsRef<Path>>(
        &mut self,
        base_dir: P,
    ) -> Result<Vec<Entry>, Box<dyn std::error::Error>> {
        let new_index = Self::new(base_dir)?;

        let mut to_delete: HashSet<Vec<u8>> = HashSet::new();
        let mut new_paths: HashMap<Vec<u8>, HashSet<PathBuf>> = HashMap::new();

        for hash in self.map.keys() {
            if let Some(entry) = new_index.map.get(hash) {
                new_paths.insert(hash.to_vec(), entry.paths.clone());
            } else {
                to_delete.insert(hash.to_vec());
            }
        }

        let mut deleted_entries: Vec<Entry> = vec![];
        for hash in to_delete.into_iter() {
            let e = self.map.remove(&hash).unwrap();
            deleted_entries.push(e);
        }

        for (k, v) in new_paths {
            let entry = self.map.get_mut(&k).unwrap();
            entry.paths = v;
        }

        Ok(deleted_entries)
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

#[derive(PartialEq)]
pub struct EncryptedIndex {
    map: HashMap<Vec<u8>, Encrypted>,
    // datadir?
}
impl EncryptedIndex {
    pub fn new<P: AsRef<Path>>(dir: P) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            map: Self::build_index(dir)?,
        })
    }

    pub fn get_entry_ref(&self, hash: &[u8]) -> Result<&Encrypted, Box<dyn std::error::Error>> {
        let e = self.map.get(hash).unwrap();
        Ok(e)
    }

    // walk the data dir and check archives against the index
    // ignore block/char specials, etc.
    pub fn build_index<P: AsRef<Path>>(
        data_dir: P,
    ) -> Result<HashMap<Vec<u8>, Encrypted>, Box<dyn std::error::Error>> {
        // println!("data_dir: {:?}", data_dir.as_ref());
        let index_file = data_dir.as_ref().join(INDEX_FILENAME);
        let mut map = HashMap::new();

        for elem in WalkDir::new(&data_dir).into_iter().filter_map(|e| e.ok()) {
            // TODO: allow symlinks?
            if !elem.file_type().is_file() {
                continue;
            }

            // filter index.dat
            if elem.path() == index_file {
                // println!("HO, HO, HO!! We found the index!!!");
                continue;
            }

            let metadata = fs::metadata(elem.path())?;
            let size = metadata.len();
            // println!("{:?}: {:?} bytes", elem.path(), size);

            // TODO: streaming reads here? as some files could be GB in size...
            let filedata = fs::read(elem.path()).unwrap();
            let mh = Code::Sha2_512.digest(&filedata);

            let _encrypted = map.entry(mh.to_bytes()).or_insert({
                Encrypted {
                    path: elem.into_path(),
                    hash: mh.to_bytes(),
                    size,
                    keys: vec![],
                }
            });
        }

        Ok(map)
    }

    // TODO: restore is a bit more tricky than imagined ... the entries in the
    // regular Index **MUST** exist, otherwise we have no path data to restore
    // to, nor do we know how to reconcile it.
    //
    //     - did it decrypt properly?
    //     - what is the hash/size of the un-encrypted file?
    //
    //  If there are any EncryptedEntries that cannot be reconciled to the plain
    //  index, those would be considered dangling. We don't know how to restore
    //  them, so to do so would be to give a best guess. It could still be done
    //  into a .restored/ dir with the plain hash as the filename and a message
    //  about what happened (dangling enc files found, restored to .restored/, etc.)
    //
    //
    // // Return a Vec<Encrypted> that exists in this EncryptedIndex, but do *not* yet exist
    // // in the plain Index.
    // //
    // // TODO: write tests for this (incl. a tX dir w/some enc files and some not,
    // // to make sure this returns the right values)
    // pub fn difference_idx<'a, 'b>(&'a self, idx: &'b Index) -> Vec<&'a Encrypted> {
    //     let mut to_decrypt: Vec<&Encrypted> = vec![];
    //     for enc in self.map.values() {
    //         match &enc.enc {
    //             None => to_decrypt.push(enc),
    //             Some(enc) => {
    //                 // if enc_idx.get_entry_ref(&enc.hash).is_err() {
    //                 //     to_decrypt.push(enc);
    //                 // }
    //             }
    //         };
    //     }
    //     to_decrypt
    // }

    // TODO: reverse of the above method -- how to get the difference when
    // enc_idx has items that don't exist in plain idx?
    // Also make tests for it
    //
    // TODO: write tests for this (incl. a tX dir w/some enc files and some not,
    // to make sure this returns the right values)
}

impl fmt::Debug for EncryptedIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = writeln!(f, "EncryptedIndex {{ map: ");
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
    use std::collections::HashSet;

    const TEST_DIR_T0: &str = "test/t0/";
    // const TEST_DIR_T1: &str = "test/t1/";
    // const TEST_DIR_T2: &str = "test/t2/";

    #[test]
    fn index() {
        let index = Index::new(TEST_DIR_T0).unwrap();
        let art1_hash = hex::decode("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6").unwrap();
        let entry = index.get_entry_ref(&art1_hash).unwrap();
        let paths = HashSet::from([
            "test/t0/art1_dup_en.txt".into(),
            "test/t0/article1_en.txt".into(),
        ]);

        assert_eq!(
            Entry {
                paths,
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
            paths: HashSet::from(["testfile.txt".into()]),
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

    const TEST_AGE_SECRET_KEY: &str =
        "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
    const TEST_DIR_T3: &str = "test/t3/";
    use crate::age::BlackBox;
    use crate::config;
    // TODO: THIS!! Ensure deleted entries are returned, and add a
    // same-hash,different-path entry for good measure.

    #[test]
    fn update_idx() {
        let cfg = config::read_config(TEST_DIR_T3).unwrap();
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut index = match cfg.load_index(TEST_DIR_T3, &bbox).unwrap() {
            None => Index::new(TEST_DIR_T3).unwrap(),
            Some(idx) => idx,
        };
        let deleted_entries = index.update(TEST_DIR_T3).unwrap();

        assert_eq!(deleted_entries, vec![Entry {
            paths: HashSet::from(["test/t3/article1_lu.txt".into()]),
            filetype: "Unicode text, UTF-8 text".to_string(),
            hash: hex::decode("13406fa591deec7fda88c97db59ee1bdbebe7d3057bb86b607b4971399a8938127ca3a39ceae6fed7b85d6a1e121ae65745a363da622e4b64ea66ff2acf250af6e6b").unwrap(),
            size: 223,
            enc: None,
            tags: vec![],
            notes: None,
        }]);

        let entries = index.get_all_entry_refs();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], &Entry {
            // paths: HashSet::from(["test/t3/article1_en.txt".into(), "test/t3/art1_dup_en.txt".into()]),
            paths: HashSet::from(["test/t3/article-one.txt".into()]),
            filetype: "ASCII text".to_string(),
            hash: hex::decode("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6").unwrap(),
            size: 171,
            enc: None,
            tags: vec![],
            notes: None,
        });
    }
}
