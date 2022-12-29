use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::{
    fs,
    io::{self},
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

use crate::age::BlackBox;
use crate::compression::{compress, decompress};
use crate::format::datetime_format;
use crate::hash::{self, Hash};
use crate::magic::Wizard;

// #[allow(unused_imports)]
use super::encrypted::{Encrypted, EncryptedIndex};
use super::entry::Entry;

pub const INDEX_FILENAME: &str = "index.dat";
const CURRENT_INDEX_VERSION: &str = "0.1.1";

#[derive(Debug, PartialEq, Serialize, Deserialize, Eq)]
pub struct Index {
    pub(crate) map: HashMap<Hash, Entry>,
    pub(crate) version: String,
    #[serde(with = "datetime_format")]
    pub(crate) created_at: NaiveDateTime,
    #[serde(with = "datetime_format")]
    pub(crate) updated_at: NaiveDateTime,
}

impl Index {
    pub fn new<P: AsRef<Path>>(dir: P) -> Result<Self, Box<dyn std::error::Error>> {
        let map = Self::build_index(dir)?;
        Ok(Index {
            version: CURRENT_INDEX_VERSION.to_string(),
            map,
            ..Default::default()
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

    pub fn get_entry_ref(&self, hash: &Hash) -> Result<&Entry, Box<dyn std::error::Error>> {
        let e = self.map.get(hash).unwrap();
        Ok(e)
    }

    pub fn get_mut_entry_ref(&mut self, hash: &Hash) -> Option<&mut Entry> {
        self.map.get_mut(hash)
    }

    // walk the dir and hash all regular files
    // ignore block/char specials, etc.
    fn build_index<P: AsRef<Path>>(
        base_dir: P,
    ) -> Result<HashMap<Hash, Entry>, Box<dyn std::error::Error>> {
        let mut map_files: HashMap<Hash, Entry> = HashMap::new();

        let wiz = Wizard::new();
        let bludir = base_dir.as_ref().join(".blu/");

        // TODO: normalize paths by trimming basedir from each elem walked ...
        for elem in WalkDir::new(&base_dir).into_iter().filter_map(|e| e.ok()) {
            // TODO: normalize path prefixes (see comment just above)
            // skip special .blu dir
            #[allow(clippy::needless_borrow)]
            if elem.path().starts_with(&bludir) {
                continue;
            }

            // TODO: allow symlinks?
            if !elem.file_type().is_file() {
                continue;
            }

            let metadata = fs::metadata(elem.path())?;
            let size = metadata.len() as usize;
            // println!("{:?}: {:?} bytes", elem.path(), size);

            // TODO: streaming reads here? as some files could be GB in size...
            let filedata = fs::read(elem.path()).unwrap();
            let filetype = wiz
                .get_filetype(&filedata, size)
                .unwrap_or_else(|_| "other".into());
            let mh = hash::multihash(&filedata);
            let hash = Hash::from(mh.to_bytes());

            // entry is a reference to the entry in the hashmap ...
            let entry = map_files.entry(hash.clone()).or_insert(Entry {
                filetype,
                paths: HashSet::new(),
                size,
                hash,
                enc: None,
                tags: vec![],
                notes: None,
            });
            // ... so when it gets modified here, it is updated in the hashmap
            entry.paths.insert(elem.into_path());
        }

        Ok(map_files)
    }

    // get all entries in the index
    pub fn get_all_entry_refs(&self) -> Vec<&Entry> {
        self.map.values().collect::<Vec<&Entry>>()
    }

    // Return a Vec of Entries that exist in this Index, but do *not* yet exist
    // in the EncIdx.
    pub fn difference_enc_idx(&self, enc_idx: &EncryptedIndex) -> Vec<Entry> {
        let mut to_encrypt: Vec<Entry> = vec![];
        for entry in self.map.values() {
            match &entry.enc {
                None => to_encrypt.push(entry.clone()),
                Some(enc) => {
                    if enc_idx.get_entry_ref(&enc.hash).is_none() {
                        to_encrypt.push(entry.clone());
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

        let mut to_delete: HashSet<Hash> = HashSet::new();
        let mut new_paths: HashMap<Hash, HashSet<PathBuf>> = HashMap::new();
        let mut is_updated = false;

        for hash in self.map.keys() {
            if let Some(entry) = new_index.map.get(hash) {
                new_paths.insert(hash.clone(), entry.paths.clone());
            } else {
                to_delete.insert(hash.clone());
            }
        }

        let mut deleted_entries: Vec<Entry> = vec![];
        for hash in to_delete.into_iter() {
            let e = self.map.remove(&hash).unwrap();
            deleted_entries.push(e);
            is_updated = true;
        }

        for (k, v) in new_paths {
            let entry = self.map.get_mut(&k).unwrap();
            if entry.paths != v {
                entry.paths = v;
                is_updated = true;
            }
        }

        // update the timestamp
        if is_updated {
            self.set_updated_timestamp();
        }

        Ok(deleted_entries)
    }

    pub fn set_updated_timestamp(&mut self) {
        self.updated_at = now();
    }
}

impl Default for Index {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
            version: CURRENT_INDEX_VERSION.to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }
}

fn serialize_index(index: &Index) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let encoded: Vec<u8> = bincode::serialize(index)?;
    // let encoded: Vec<u8> = serde_cbor::to_vec(index)?;
    Ok(encoded)
}

fn deserialize_index(data: &[u8]) -> Result<Index, Box<dyn std::error::Error>> {
    // let decoded: Index = serde_cbor::from_slice(data)?;
    let decoded: Index = match bincode::deserialize(data) {
        Ok(index) => index,
        Err(_) => OldIndex::deserialize(data)?.into_index(),
    };
    Ok(decoded)
}

fn now() -> chrono::NaiveDateTime {
    // returns a NaiveDateTime without milli/nano seconds
    NaiveDateTime::from_timestamp(chrono::Utc::now().timestamp(), 0)
}

// This struct is only used to deserialize and convert into a new index with
// timestamps.
#[derive(PartialEq, Serialize, Deserialize, Eq)]
pub struct OldIndex {
    map: HashMap<Hash, Entry>,
    version: String,
}
impl OldIndex {
    pub fn deserialize(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        let decoded: OldIndex = bincode::deserialize(data)?;
        Ok(decoded)
    }
    pub fn into_index(self) -> Index {
        let (map, _version) = (self.map, self.version);
        Index {
            map,
            version: CURRENT_INDEX_VERSION.to_string(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod test {
    use super::{
        compress, deserialize_index, serialize_index, Encrypted, EncryptedIndex, Entry, HashMap,
        Index,
    };
    use crate::hash::{self, Hash};
    use std::collections::HashSet;

    const TEST_DIR_T0: &str = "test/t0/";
    // const TEST_DIR_T1: &str = "test/t1/";
    // const TEST_DIR_T2: &str = "test/t2/";

    #[test]
    fn index() {
        let index = Index::new(TEST_DIR_T0).unwrap();
        let art1_hash = Hash::from("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6");
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
        let hash = Hash::from(hash::multihash(b).to_bytes());
        Entry {
            paths: HashSet::from(["testfile.txt".into()]),
            filetype: "ASCII text".to_string(),
            size: b.len(),
            hash,
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
            ..Default::default()
        };
        let serialized_idx = serialize_index(&index).unwrap();
        // println!(
        //     "{} (len {} bytes)",
        //     &hex::encode(&serialized_idx),
        //     serialized_idx.len()
        // );

        let _compressed_ser_idx = compress(&serialized_idx).unwrap();
        // println!(
        //     "compressed: {} (len {} bytes)",
        //     &hex::encode(&compressed_ser_idx),
        //     _compressed_ser_idx.len()
        // );

        let idx2 = deserialize_index(&serialized_idx).unwrap();
        assert_eq!(index, idx2);
    }

    const TEST_AGE_SECRET_KEY: &str =
        "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
    const TEST_DIR_T3: &str = "test/t3/";
    use crate::age::BlackBox;
    use crate::config;

    #[test]
    fn update_idx() {
        let cfg = config::read_config(TEST_DIR_T3).unwrap();
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut index = match cfg.v1_load_index(&bbox).unwrap() {
            None => Index::new(TEST_DIR_T3).unwrap(),
            Some(idx) => idx,
        };
        let deleted_entries = index.update(TEST_DIR_T3).unwrap();

        assert_eq!(
            deleted_entries,
            vec![Entry {
                paths: HashSet::from(["test/t3/article1_lu.txt".into()]),
                filetype: "Unicode text, UTF-8 text".to_string(),
                hash: Hash::from("13406fa591deec7fda88c97db59ee1bdbebe7d3057bb86b607b4971399a8938127ca3a39ceae6fed7b85d6a1e121ae65745a363da622e4b64ea66ff2acf250af6e6b"),
                size: 223,
                enc: None,
                tags: vec![],
                notes: None,
            }]
        );

        let entries = index.get_all_entry_refs();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], &Entry {
            paths: HashSet::from(["test/t3/article-one.txt".into()]),
            filetype: "ASCII text".to_string(),
            hash: Hash::from("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6"),
            size: 171,
            enc: None,
            tags: vec![],
            notes: None,
        });
    }

    // Return a Vec of Entries that exist in this Index, but do *not* yet exist
    // in the EncIdx.
    const TEST_DIR_T4: &str = "test/t4/";
    #[test]
    fn diff_enc_idx() {
        // load index
        let cfg = config::read_config(TEST_DIR_T4).unwrap();
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut index = match cfg.v1_load_index(&bbox).unwrap() {
            None => Index::new(TEST_DIR_T4).unwrap(),
            Some(idx) => idx,
        };
        let _deleted_entries = index.update(TEST_DIR_T4).unwrap();
        // dbg!(&_deleted_entries);

        // get the difference w/EncryptedIndex dir
        let enc_idx = EncryptedIndex::new(cfg.datadir()).unwrap();
        // dbg!(&enc_idx);

        // get the entries to be encrypted
        let to_encrypt = index.difference_enc_idx(&enc_idx);
        // dbg!(&to_encrypt);

        assert_eq!(
            to_encrypt,
            vec![
                Entry {
                    paths: HashSet::from(["test/t4/article1_lu.txt".into()]),
                    filetype: "Unicode text, UTF-8 text".to_string(),
                    hash: Hash::from("13406fa591deec7fda88c97db59ee1bdbebe7d3057bb86b607b4971399a8938127ca3a39ceae6fed7b85d6a1e121ae65745a363da622e4b64ea66ff2acf250af6e6b"),
                    size: 223,
                    enc: None,
                    tags: vec![],
                    notes: None,
                }
            ]
        );
    }

    const TEST_DIR_T5: &str = "test/t5/";
    #[test]
    fn diff_idx() {
        // load index
        let cfg = config::read_config(TEST_DIR_T5).unwrap();
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut index = match cfg.v1_load_index(&bbox).unwrap() {
            None => Index::new(TEST_DIR_T5).unwrap(),
            Some(idx) => idx,
        };
        // dbg!(&index);

        let deleted_entries = index.update(TEST_DIR_T5).unwrap();
        // dbg!(&deleted_entries);
        assert_eq!(deleted_entries.len(), 0);

        // get the difference w/EncryptedIndex dir
        let enc_idx = EncryptedIndex::new(cfg.datadir()).unwrap();
        // dbg!(&enc_idx);

        // get dangling entries
        let (dangling, _dup_enc_hashes) = enc_idx.difference_idx(&mut index, Some(&bbox)).unwrap();
        // dbg!(&dangling);

        assert_eq!(
            dangling,
            vec![
                &Encrypted {
                    path: "test/t5/.blu/data/9/9b1/9b1d7/9b1d7ad7a63e3931b2547c3534962dbae82607d4264f8fbdc22526b2576dd6b58e52d4b770319862568c10cf44d0278a00bebc6e9c78c9f9a3b09894aa07daed".into(),
                    hash: Hash::from("13409b1d7ad7a63e3931b2547c3534962dbae82607d4264f8fbdc22526b2576dd6b58e52d4b770319862568c10cf44d0278a00bebc6e9c78c9f9a3b09894aa07daed"),
                    size: 563,
                    keys: vec![],
                },
            ]
        );
    }

    // test multiple different Encrypted's that decrypt to the same file
    // (reconciliation / convergence (upon a single enc hash) / cleanup)
    const TEST_DIR_T6: &str = "test/t6/";
    #[test]
    fn double_enc() {
        // load index
        let cfg = config::read_config(TEST_DIR_T6).unwrap();
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut index = cfg
            .v1_load_index(&bbox)
            .unwrap()
            .unwrap_or_else(|| Index::new(TEST_DIR_T6).unwrap());

        // ensure the index changes after reconciliation + convergence
        let en_hash = Hash::from("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6");
        let entry_ref = index.get_entry_ref(&en_hash).unwrap();
        let enc = entry_ref.get_enc().unwrap();
        // initially this is the enc hash ...
        assert_eq!(enc.hash, Hash::from("13402e3612c3ac8d4322d1345d4cdb798bf0fb280ffe77b8f3e19e1bb745b1ee80dd9a1ec07fed6b0876456ffc91f48b65fd79565189fe3447d31b2da42ba32528e3"));

        // walk enc datadir and index
        let enc_idx = EncryptedIndex::new(cfg.datadir()).unwrap();
        // reconciliation + convergence -- this modifies the index
        let (_dangling, dup_enc_entries) = enc_idx.difference_idx(&mut index, Some(&bbox)).unwrap();
        assert_eq!(dup_enc_entries[0].hash, Hash::from("13402e3612c3ac8d4322d1345d4cdb798bf0fb280ffe77b8f3e19e1bb745b1ee80dd9a1ec07fed6b0876456ffc91f48b65fd79565189fe3447d31b2da42ba32528e3"));

        // ensure the index changes after reconciliation + convergence
        let entry_ref = index.get_entry_ref(&en_hash).unwrap();
        let enc = entry_ref.get_enc().unwrap();
        // ... enc hash changes to this after convergence
        assert_eq!(enc.hash, Hash::from("13402d982fd888d1456987cc4fc88dce3e87aba1248b49c78c03d7933efbafebb77f6b2ae3d8ceb565e52feb168e39a10dafcf30c0087e451d5bec8fa2f1f3e8532e"));
    }
}
