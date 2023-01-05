// #![warn(rust_2018_idioms)]
// #![warn(missing_debug_implementations)]
// #![warn(missing_docs)]
//
// https://doc.rust-lang.org/rustc/lints/groups.html

use clap::Parser;
use std::{env, process};

#[macro_use]
extern crate log;

pub mod age;
pub mod blob;
pub mod block;
pub mod clapargs;
pub mod compression;
pub mod config;
pub mod dir;
pub mod format;
pub mod hash;
pub mod io;
pub mod magic;
pub mod metadata;
pub mod tagger;

pub mod cmds;

use crate::age::BlackBox;

const TEST_AGE_SECRET_KEY: &str = include_str!("../test/blu_secrets/blu.key");

// also: consider an internal webserver which serves up the UI for blu

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: clapargs::Args = clapargs::Args::parse();

    // TODO: use pwd
    let dir = env::var("BLU_DIR").unwrap_or_else(|_| {
        println!("error: required env var BLU_DIR is not set");
        process::exit(1);
    });
    dbg!(&dir);

    let cfg = config::read_config(&dir)?;
    dbg!(&cfg);

    let bbox = load_key();
    dbg!(&bbox);
    // let mut index = match cfg.load_index(&bbox)? {
    //     None => Index::new(&dir)?,
    //     Some(idx) => idx,
    // };
    // let mut index = Index::new(dir)?;

    match args.action {
        // There are 2 basic operations:
        //     a. archive - encrypt+de-duplicate new files
        //     b. restore - restore from backup
        //
        clapargs::Action::Add => {
            cmds::add();
        }
        clapargs::Action::Init => {
            cmds::init();
        }
        clapargs::Action::Restore => {
            cmds::restore();
        }
        clapargs::Action::ListTags => {
            cmds::list_tags(&cfg, &bbox);
        }
        _ => {
            unimplemented!();
        }
    };

    Ok(())
}

// TODO: Rename/multi keys
fn load_key() -> BlackBox {
    BlackBox::new(&[TEST_AGE_SECRET_KEY])
}

//fn old_enc() {
//    // Consider the case in which we load the index from disk as above, but
//    // entries are either added to or deleted from the disk. The index will have
//    // to be updated to reflect this. Something like:
//    //
//    let deleted_entries = index.update(dir)?;
//    dbg!(&deleted_entries);
//    //
//    // What do we do with files which were removed from the disk?
//    //
//    // Delete them from encrypted archive also? Or leave them to dangle?
//    //
//    //
//    //
//    //
//    // Note: this is one form of "dangling"
//    //
//    // the other way is to crawl enc dir (EncryptedIndex) and attempt to
//    // reconcile back to the index. If no reconciliation is possible (no hash
//    // matches for decrypted data), then those are truly "dangling".
//    let datadir = cfg.datadir();
//    let dir_manager = dir::Manager::new(&datadir);
//    if cfg.prune_dangling {
//        for entry in deleted_entries.into_iter() {
//            if let Some(enc) = entry.get_enc() {
//                dir_manager.delete_encrypted(&enc)?;
//            }
//        }
//    }
//
//    // TODO: _iff_ we want to chdir before indexing, **HERE** is where
//    // let index = Index::new(dir)?;
//    // TODO: ... and HERE is where to change back
//
//    let enc_idx = EncryptedIndex::new(&datadir)?;
//    dbg!(&enc_idx);
//
//    // There are 2 operations:
//    //     a. archive - encrypt+de-duplicate new files
//    //     b. restore - restore from backup
//    //
//    // now, difference method depends on the operation...
//    //
//    // if we are doing in archive (encrypted any new files), then we want to get
//    // the difference of:
//    //
//    // index - enc_idx
//    // ... ignoring any extra encrypted files lying around.
//    //
//    // Likewise, a restore operation would be the opposite.
//    // enc_idx - index
//    // ... restore any left over, ignoring un-encrypted files lying around.
//
//    // get difference:
//    let to_encrypt = index.difference_enc_idx(&enc_idx);
//    dbg!(&to_encrypt);
//
//    let mut index_updated: bool = false;
//    for entry in to_encrypt.iter() {
//        // read file data from entry and encrypt it . Need to read one of the paths
//        let unenc_filedata = entry.read_filedata()?;
//        let enc_filedata = bbox.encrypt(&unenc_filedata)?;
//
//        let enc_mh_bytes = hash::multihash(&enc_filedata).to_bytes();
//        let size = enc_mh_bytes.len();
//        let enc_hash = Hash::from(enc_mh_bytes);
//        let enc_path = dir_manager.write_data(&enc_hash, &enc_filedata)?;
//
//        let entry_hash = entry.get_hash();
//
//        // we know this exists because that's how it got into `to_encrypt` in
//        // the first place
//        let e = index.get_mut_entry_ref(&entry_hash).unwrap();
//        let enc = Encrypted {
//            path: enc_path,
//            hash: enc_hash,
//            size,
//            keys: vec![],
//        };
//        e.set_encrypted(enc)?;
//        index_updated = true;
//    }
//    if index_updated {
//        index.set_updated_timestamp();
//    }
//
//    dbg!(&index);
//    // TODO: add this to either metadata, dir, config
//    let index_filename = &datadir.join(INDEX_FILENAME);
//    dbg!(&index_filename);
//
//    let mut enc_idx_bytes = Vec::new();
//    index.write(&mut enc_idx_bytes, &bbox)?;
//    let mut file = fs::File::create(index_filename)?;
//    file.write_all(&enc_idx_bytes)?;
//}
