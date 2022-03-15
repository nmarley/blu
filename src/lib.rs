use std::env;

pub mod age;
pub mod clap;
pub mod config;
pub mod dir;
pub mod hash;
pub mod magic;
pub mod metadata;

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
use crate::age::BlackBox;

// also: consider an internal webserver which serves up the UI for blu
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // TODO: handle cmd-line args w/clap
    // let mut args: Args = Args::parse();
    // if args.num_crawlers < 1 || args.num_crawlers > 999 {
    //     args.num_crawlers = 96; // how to get default here?
    // }
    // dbg!(&args);

    // let key = read-key-from-.blu/metadata.json;
    // decrypt somehow?

    let args: Vec<String> = env::args().collect();
    let dir = &args[1];

    let cfg = config::read_config(dir)?;
    // dbg!(&cfg);

    let abs_datadir = std::path::Path::new(dir).join(cfg.datadir());
    // dbg!(&abs_datadir);

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let mut index = match cfg.load_index(dir, &bbox)? {
        None => metadata::Index::new(dir)?,
        Some(idx) => idx,
    };
    // let mut index = metadata::Index::new(dir)?;

    // Consider the case in which we load the index from disk as above, but
    // entries are either added to or deleted from the disk. The index will have
    // to be updated to reflect this. Something like:
    //
    let deleted_entries = index.update(dir)?;
    dbg!(&deleted_entries);
    //
    // What do we do with files which were removed from the disk?
    //
    // Delete them from encrypted archive also? Or leave them to dangle?
    //
    //
    //
    //
    // Note: this is one form of "dangling"
    //
    // the other way is to crawl enc dir (EncryptedIndex) and attempt to
    // reconcile back to the index. If no reconciliation is possible (no hash
    // matches for decrypted data), then those are truly "dangling".
    let dir_manager = dir::Manager::new(&abs_datadir);
    if cfg.prune_dangling {
        for mut entry in deleted_entries.into_iter() {
            if let Some(enc) = entry.get_enc() {
                dir_manager.delete_encrypted(&enc)?;
            }
        }
    }

    // TODO: _iff_ we want to chdir before indexing, **HERE** is where
    // let index = metadata::Index::new(dir)?;
    // TODO: ... and HERE is where to change back

    let enc_idx = metadata::EncryptedIndex::new(&abs_datadir)?;
    dbg!(&enc_idx);

    // There are 2 operations:
    //     a. archive - encrypt+de-duplicate new files
    //     b. restore - restore from backup
    //
    // now, difference method depends on the operation...
    //
    // if we are doing in archive (encrypted any new files), then we want to get
    // the difference of:
    //
    // index - enc_idx
    // ... ignoring any extra encrypted files lying around.
    //
    // Likewise, a restore operation would be the opposite.
    // enc_idx - index
    // ... restore any left over, ignoring un-encrypted files lying around.

    // get difference:
    let to_encrypt = index.difference_enc_idx(&enc_idx);
    dbg!(&to_encrypt);

    for entry in to_encrypt.iter() {
        // read file data from entry and encrypt it . Need to read one of the paths
        let unencrypted_filedata = entry.read_filedata()?;
        let encrypted_filedata = bbox.encrypt(&unencrypted_filedata)?;
        match dir_manager.write_encrypted(&encrypted_filedata) {
            Err(e) => {
                eprintln!("error: {}", e);
            }
            Ok(enc) => {
                entry.set_encrypted(enc);
            }
        };
    }

    Ok(())
}
