use std::io::Write;
use std::{env, fs};

pub mod age;
pub mod clap;
pub mod config;
pub mod magic;
pub mod metadata;

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
use crate::age::BlackBox;

use crate::metadata::Entry;

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
    let index = match cfg.load_index(dir, &bbox)? {
        None => metadata::Index::new(dir)?,
        Some(idx) => idx,
    };
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

    // ... now, how to get difference?
    let mut to_encrypt: Vec<&Entry> = vec![];
    for entry in index.map.values() {
        match &entry.enc {
            None => to_encrypt.push(entry),
            Some(enc) => {
                match enc_idx.get_entry_ref(&enc.hash) {
                    Ok(_) => {} // all good
                    Err(_) => to_encrypt.push(entry),
                }
            }
        };
    }
    dbg!(&to_encrypt);

    // writing index for testing
    // let mut enc_idx = Vec::new();
    // let _ = index.write(&mut enc_idx, &bbox)?;
    // let mut file = fs::File::create("test-idx-enc.dat")?;
    // file.write_all(&enc_idx)?;

    Ok(())
}
