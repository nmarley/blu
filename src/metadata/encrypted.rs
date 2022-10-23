use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::{
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

use crate::age::BlackBox;
use crate::config::KeyID;
use crate::hash::{self, Hash};

use super::{Index, INDEX_FILENAME};

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Hash, Eq, PartialOrd, Ord)]
pub struct Encrypted {
    // in theory, there won't be multiple files in the encrypted datadir with
    // the same hash
    pub path: PathBuf,
    pub hash: Hash,
    pub size: usize,
    pub keys: Vec<KeyID>,
}

impl Encrypted {
    pub fn get_hash(&self) -> Hash {
        self.hash.clone()
    }

    pub fn get_hash_ref(&self) -> &Hash {
        &self.hash
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct EncryptedIndex {
    map: HashMap<Hash, Encrypted>,
}

type PairVecEncRef<'a, Encrypted> = (Vec<&'a Encrypted>, Vec<&'a Encrypted>);

impl EncryptedIndex {
    pub fn new<P: AsRef<Path>>(dir: P) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            map: Self::build_index(dir)?,
        })
    }

    pub fn get_entry_ref(&self, hash: &Hash) -> Option<&Encrypted> {
        self.map.get(hash)
    }

    // walk the data dir and check archives against the index
    // ignore block/char specials, etc.
    pub fn build_index<P: AsRef<Path>>(
        data_dir: P,
    ) -> Result<HashMap<Hash, Encrypted>, Box<dyn std::error::Error>> {
        // println!("data_dir: {:?}", data_dir.as_ref());
        let index_file = data_dir.as_ref().join(INDEX_FILENAME);
        let mut map: HashMap<Hash, Encrypted> = HashMap::new();

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
            let size = metadata.len() as usize;
            // println!("{:?}: {:?} bytes", elem.path(), size);

            // TODO: streaming reads here? as some files could be GB in size...
            let filedata = fs::read(elem.path()).unwrap();
            let mh = hash::multihash(&filedata);
            let hash = Hash::from(mh.to_bytes());

            let _encrypted = map.entry(hash.clone()).or_insert({
                Encrypted {
                    path: elem.into_path(),
                    hash,
                    size,
                    keys: vec![],
                }
            });
        }

        Ok(map)
    }

    // restore is a bit more tricky than imagined ... the entries in the
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
    pub fn difference_idx<'a, 'b, 'c>(
        &'a self,
        idx: &'b mut Index,
        opt_bbox: Option<&'c BlackBox>,
    ) -> Result<PairVecEncRef<'a, Encrypted>, Box<dyn std::error::Error>> {
        // list of Encrypted's not found in the Index
        let mut not_found: HashSet<Hash> = HashSet::new();

        // ensure doubly encrypted files are reported / can be cleaned up
        // plain_hash -> hashset(enc hash)
        let mut map_plain_enc_set: HashMap<Hash, HashSet<Hash>> = HashMap::new();
        // enc hash -> plain hash mapping
        let mut idx_enchash_plainhash: HashMap<Hash, Hash> = HashMap::new();
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
        for enchash in self.map.keys() {
            if !idx_enchash_plainhash.contains_key(enchash) {
                not_found.insert(enchash.clone());
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
                // decrypt it ...
                let enc = self.map.get(&hash).unwrap();
                let enc_filedata = fs::read(&enc.path)?;
                let filedata = bbox.decrypt(&enc_filedata)?;
                let h2 = Hash::from(hash::multihash(&filedata).to_bytes());
                // reconciliation happens here
                if let Some(entry) = idx.get_mut_entry_ref(&h2) {
                    // hashset (do not assume unique enc hashes in the index)
                    let hs = map_plain_enc_set
                        .entry(entry.hash.clone())
                        .or_insert_with(HashSet::new);
                    if (*entry.get_enc_ref()).is_none() {
                        entry.set_encrypted(enc.clone())?;
                    }
                    hs.insert(enc.hash.clone());
                    // reconcile succeeded.
                } else {
                    dangling.push(enc);
                }
            }
        }

        // converge upon a single enc hash value if multiple found
        let mut old_dup_encrypted: Vec<&Encrypted> = Vec::new();
        for (plain_hash, set_enc) in map_plain_enc_set.into_iter() {
            if set_enc.len() > 1 {
                let mut v: Vec<_> = set_enc.iter().collect();
                v.sort();
                let mut v_iter = v.into_iter();
                let top_enc_hash = v_iter.next().unwrap();

                // this is so screwy ...
                for item in v_iter {
                    // Due to the lifetime constraints, it's necessary to return
                    // a ref from self, not idx. `item` here is a ref to idx.
                    old_dup_encrypted.push(self.get_entry_ref(item).unwrap())
                }

                // update index iff highest enc hash not used
                if let Some(e) = idx.get_mut_entry_ref(&plain_hash) {
                    if let Some(enc) = e.get_enc_ref() {
                        if &enc.hash != top_enc_hash {
                            // e.set_encrypted((*self.get_entry_ref(top_enc_hash)?).clone())?;
                            let top_enc = self.get_entry_ref(top_enc_hash).unwrap().clone();
                            e.set_encrypted(top_enc)?;
                        }
                    }
                }
            }
        }

        // // old_dup_encrypted
        // println!("\nold_dup_encrypted:");
        // for v in old_dup_encrypted.iter() {
        //     dbg!(hex::encode(v));
        // }
        // println!("\n");

        // `dangling` are the enc entries which cannot be reconciled to any file
        // data in the plain index, meaning we don't have a file name or other
        // metadata to link with.
        //
        // `old_dup_encrypted` contains the encrypted entries which are
        // redundant. They shouldn't be referenced anywhere and should be able
        // to be cleaned up (removed from disk).
        Ok((dangling, old_dup_encrypted))
    }
}
