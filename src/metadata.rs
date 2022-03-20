use chrono::NaiveDateTime;
use flate2::bufread::{GzDecoder, GzEncoder};
use flate2::Compression;
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
use crate::format::datetime_format;
use crate::hash;
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
    pub fn get_enc_ref(&self) -> &Option<Encrypted> {
        &self.enc
    }

    pub fn get_enc(&self) -> Option<Encrypted> {
        self.enc.clone()
    }

    pub fn read_filedata(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let path = self.paths.iter().next().unwrap();
        let data = fs::read(path)?;
        Ok(data)
    }

    pub fn set_encrypted(&mut self, enc: Encrypted) -> Result<(), Box<dyn std::error::Error>> {
        self.enc = Some(enc);
        Ok(())
    }

    pub fn get_hash(&self) -> Vec<u8> {
        self.hash.clone()
    }
}

#[derive(PartialEq, Serialize, Deserialize, Clone)]
pub struct Encrypted {
    // in theory, there won't be multiple files in the encrypted datadir with
    // the same hash
    pub path: PathBuf,
    pub hash: Vec<u8>,
    pub size: u64,
    pub keys: Vec<KeyID>,
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
    // let decoded: Index = serde_cbor::from_slice(data)?;
    let decoded: Index = match bincode::deserialize(data) {
        Ok(index) => index,
        Err(_) => OldIndex::deserialize(data)?.into_index(),
    };
    Ok(decoded)
}

// This struct is only used to deserialize and convert into a new index with
// timestamps.
#[derive(PartialEq, Serialize, Deserialize)]
pub struct OldIndex {
    map: HashMap<Vec<u8>, Entry>,
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
    #[serde(with = "datetime_format")]
    created_at: NaiveDateTime,
    #[serde(with = "datetime_format")]
    updated_at: NaiveDateTime,
}

const CURRENT_INDEX_VERSION: &str = "0.1.1";
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

    pub fn get_entry_ref(&self, hash: &[u8]) -> Result<&Entry, Box<dyn std::error::Error>> {
        let e = self.map.get(hash).unwrap();
        Ok(e)
    }

    pub fn get_mut_entry_ref(&mut self, hash: &[u8]) -> Option<&mut Entry> {
        self.map.get_mut(hash)
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
            let mh = hash::hash(&filedata);

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
    pub fn difference_enc_idx(&self, enc_idx: &EncryptedIndex) -> Vec<Entry> {
        let mut to_encrypt: Vec<Entry> = vec![];
        for entry in self.map.values() {
            match &entry.enc {
                None => to_encrypt.push(entry.clone()),
                Some(enc) => {
                    if enc_idx.get_entry_ref(&enc.hash).is_err() {
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

        let mut to_delete: HashSet<Vec<u8>> = HashSet::new();
        let mut new_paths: HashMap<Vec<u8>, HashSet<PathBuf>> = HashMap::new();
        let mut is_updated = false;

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

impl fmt::Debug for Index {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // let _ = writeln!(f, "Index {{ version: {}, map: ", &self.version);
        let _ = writeln!(f, "Index {{");
        let _ = writeln!(f, "  version: {},", &self.version);
        let _ = writeln!(f, "  created_at: {},", &self.created_at);
        let _ = writeln!(f, "  updated_at: {},", &self.updated_at);
        let _ = writeln!(f, "  map: ");
        for (k, v) in self.map.iter() {
            let _ = write!(f, "\n{}:\n{:?},\n", &hex::encode(k), v);
        }
        let _ = write!(f, "}}");
        Ok(())
    }
}

fn now() -> chrono::NaiveDateTime {
    // returns a NaiveDateTime without milli/nano seconds
    NaiveDateTime::from_timestamp(chrono::Utc::now().timestamp(), 0)
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
            let mh = hash::hash(&filedata);

            // TODO: the only way to get hashes of the un-encrypted data here is
            // to decrypt and hash

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

    // Return a Vec<Encrypted> that exists in this EncryptedIndex, but do *not*
    // exist in the plain Index.

    // If they don't exist in the plain Index, but they _do_ exist in the
    // EncryptedIndex, then they are considered dangling Encrypted.
    // They can be restored to special .restored, but don't have a filename in
    // the plain dir or any tags / notes.

    // If they exist in the plain Index, and also in the EncryptedIndex, then
    // they can be restored, which only makes sense if the files don't exist on
    // the filesystem.
    //
    // Note that this operation shouldn't need a special "difference" case -- it
    // is the on the happy path. Just walk each entry and restore (decrypt)
    // _iff_ it isn't on the filesystem.

    // Reconciliation is a special case in which the plain Index entries exist
    // but without a Encrypted to point to (enc set to None), AND ... there is a
    // matching Encrypted entry on-disk which can decrypt to match the plain
    // hash.

    // TODO: write tests for this (incl. a tX dir w/some enc files and some not,
    // to make sure this returns the right values)
    //
    // TODO: also consider the case when multiple different encrypted versions
    // of the same plain files exist... clean up?
    //
    // TODO: split out reconciliation into own fn?
    pub fn difference_idx<'a, 'b, 'c>(
        &'a self,
        idx: &'b mut Index,
        opt_bbox: Option<&'c BlackBox>,
    ) -> Result<Vec<&'a Encrypted>, Box<dyn std::error::Error>> {
        // list of Encrypted's not found in the Index
        let mut not_found: HashSet<Vec<u8>> = HashSet::new();

        // plain_hash -> hashset(enc hash)
        // ensure doubly encrypted files are reported / can be cleaned up
        let mut map_plain_enc_set: HashMap<Vec<u8>, HashSet<Vec<u8>>> = HashMap::new();

        // TODO: should this be a method on Index?
        let mut idx_enchash_plainhash: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
        for entry in idx.map.values() {
            if let Some(enc) = &entry.enc {
                // hashset (do not assume unique enc hashes in the index)
                let hs = map_plain_enc_set
                    .entry(entry.hash.clone())
                    .or_insert_with(HashSet::new);
                hs.insert(enc.hash.clone());

                idx_enchash_plainhash.insert(enc.hash.clone(), entry.hash.clone());
            }
        }

        // // dbg!(&idx_enchash_plainhash);
        // println!("\nidx_enchash_plainhash:");
        // for (k, v) in idx_enchash_plainhash.iter() {
        //     dbg!(hex::encode(k), hex::encode(v));
        // }
        // println!("\n");

        // not_found is candidate for reconciliation or dangling
        for k in self.map.keys() {
            if !idx_enchash_plainhash.contains_key(k) {
                not_found.insert(k.to_vec());
            }
        }

        // // dbg!(&not_found);
        // println!("\nnot_found:");
        // for v in not_found.iter() {
        //     dbg!(hex::encode(v));
        // }
        // println!("\n");

        // Reconciliation (decrypt to try and discover unknown mappings) if a
        // BlackBox passed in, then try and decrypt for reconciliation
        let mut dangling: Vec<&Encrypted> = vec![];

        if let Some(bbox) = opt_bbox {
            for hash in not_found.into_iter() {
                // dbg!(hex::encode(&hash));
                // decrypt it ...
                let enc = self.map.get(&hash).unwrap();
                let enc_filedata = fs::read(&enc.path)?;
                let filedata = bbox.decrypt(&enc_filedata)?;
                let mh = hash::hash(&filedata);
                // reconciliation happens here
                if let Some(entry) = idx.get_mut_entry_ref(&mh.to_bytes()) {
                    // hashset (do not assume unique enc hashes in the index)
                    let hs = map_plain_enc_set
                        .entry(entry.hash.clone())
                        .or_insert_with(HashSet::new);

                    // in theory it will never happen because hs is populated
                    // with all the Some(enc)'s earlier.
                    //
                    // I think only one of these conditions is necessary, would
                    // prefer the entry.get_enc_ref one and just use hs to keep
                    // track of duplicated enc hashes
                    if hs.is_empty() && (*entry.get_enc_ref()).is_none() {
                        entry.set_encrypted(enc.clone())?;
                    }
                    hs.insert(enc.hash.clone());
                    // reconcile succeeded.
                } else {
                    dangling.push(enc);
                }
            }
        }

        // TODO: also return multiply-encrypted items (values in
        // map_enc_plain_set with multiple entries).
        // let duplicate_encrypted_hashes = HashSet::new();
        // map_plain_enc_set => sort values and use the top one in index.

        let mut old_dup_enc_hashes: Vec<Vec<u8>> = Vec::new();
        for (plain_hash, set_enc) in map_plain_enc_set.iter() {
            if set_enc.len() > 1 {
                // top_enc_hash = (*set_enc).to_vec().sort();
                let mut v: Vec<_> = set_enc.iter().collect();
                v.sort();
                let mut v_iter = v.into_iter();
                let top_enc_hash = v_iter.next().unwrap();

                // this is so screwy ...
                for item in v_iter {
                    old_dup_enc_hashes.push((*item.clone()).to_vec());
                }

                // update index iff highest enc hash not used
                if let Some(e) = idx.get_mut_entry_ref(plain_hash) {
                    if let Some(enc) = e.get_enc_ref() {
                        if enc.hash != *top_enc_hash {
                            e.set_encrypted((*self.get_entry_ref(top_enc_hash)?).clone())?;
                        }
                    }
                }
            }
        }

        // println!("\nold_dup_enc_hashes:");
        // for v in old_dup_enc_hashes.iter() {
        //     dbg!(hex::encode(v));
        // }
        // println!("\n");

        // TODO: test for doubly-encrypted entries with different enc hashes
        // ALREADY in the index. Need some way to reconcile / converge upon only
        // one and remove the others. This would be part of `cleanup` of the
        // encrypted disk portion.

        Ok(dangling)
    }
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
    use super::{
        compress, deserialize_index, serialize_index, Encrypted, EncryptedIndex, Entry, HashMap,
        Index,
    };
    use crate::hash;
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
        let mh = hash::hash(b);
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
            ..Default::default()
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
        let mut index = match cfg.load_index(&bbox).unwrap() {
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
            paths: HashSet::from(["test/t3/article-one.txt".into()]),
            filetype: "ASCII text".to_string(),
            hash: hex::decode("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6").unwrap(),
            size: 171,
            enc: None,
            tags: vec![],
            notes: None,
        });
    }

    // Return a Vec of Entries that exist in this Index, but do *not* yet exist
    // in the EncIdx.
    //
    // TODO: write tests for this (incl. a tX dir w/some enc files and some not,
    // to make sure this returns the right values)
    const TEST_DIR_T4: &str = "test/t4/";
    #[test]
    fn diff_enc_idx() {
        // load index
        let cfg = config::read_config(TEST_DIR_T4).unwrap();
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut index = match cfg.load_index(&bbox).unwrap() {
            None => Index::new(TEST_DIR_T4).unwrap(),
            Some(idx) => idx,
        };
        let _deleted_entries = index.update(TEST_DIR_T4).unwrap();
        // dbg!(&_deleted_entries);

        // TODO: get the difference w/EncryptedIndex dir
        let enc_idx = EncryptedIndex::new(cfg.datadir()).unwrap();
        // dbg!(&enc_idx);

        // get the entries to be encrypted
        let to_encrypt = index.difference_enc_idx(&enc_idx);
        // dbg!(&to_encrypt);

        assert_eq!(to_encrypt, vec![
            Entry {
                paths: HashSet::from(["test/t4/article1_lu.txt".into()]),
                filetype: "Unicode text, UTF-8 text".to_string(),
                hash: hex::decode("13406fa591deec7fda88c97db59ee1bdbebe7d3057bb86b607b4971399a8938127ca3a39ceae6fed7b85d6a1e121ae65745a363da622e4b64ea66ff2acf250af6e6b").unwrap(),
                size: 223,
                enc: None,
                tags: vec![],
                notes: None,
            }
        ]);
    }

    const TEST_DIR_T5: &str = "test/t5/";
    #[test]
    fn diff_idx() {
        // load index
        let cfg = config::read_config(TEST_DIR_T5).unwrap();
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let mut index = match cfg.load_index(&bbox).unwrap() {
            None => Index::new(TEST_DIR_T5).unwrap(),
            Some(idx) => idx,
        };
        // dbg!(&index);

        let deleted_entries = index.update(TEST_DIR_T5).unwrap();
        // dbg!(&deleted_entries);
        assert_eq!(deleted_entries.len(), 0);

        // TODO: get the difference w/EncryptedIndex dir
        let enc_idx = EncryptedIndex::new(cfg.datadir()).unwrap();
        // dbg!(&enc_idx);

        // get dangling entries
        // TODO: how to handle duplicate encrypted from this same fn?
        let dangling = enc_idx.difference_idx(&mut index, Some(&bbox)).unwrap();
        // dbg!(&dangling);

        assert_eq!(dangling, vec![
            &Encrypted {
                path: "test/t5/.blu/data/9/9b1/9b1d7/9b1d7ad7a63e3931b2547c3534962dbae82607d4264f8fbdc22526b2576dd6b58e52d4b770319862568c10cf44d0278a00bebc6e9c78c9f9a3b09894aa07daed".into(),
                hash: hex::decode("13409b1d7ad7a63e3931b2547c3534962dbae82607d4264f8fbdc22526b2576dd6b58e52d4b770319862568c10cf44d0278a00bebc6e9c78c9f9a3b09894aa07daed").unwrap(),
                size: 563,
                keys: vec![],
            },
        ]);
    }

    // TODO: test multiple different Encrypted's that decrypt to the same file
    // (reconciliation / convergence (upon a single enc hash) / cleanup)
}
