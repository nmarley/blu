use std::env;

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
use blu::age::BlackBox;
use blu::config;
use blu::dir::Manager;
use blu::hash;
use blu::metadata::{EncryptedIndex, Index};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <dir-to-index>", args.next().unwrap());
        std::process::exit(1);
    }
    let dir = &args.nth(1).unwrap();

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);

    let cfg = config::read_config(dir)?;
    dbg!(&cfg);

    let index = match cfg.load_index(&bbox)? {
        None => Index::new(dir)?,
        Some(idx) => idx,
    };
    // let mut index = Index::new(dir)?;
    dbg!(&index);

    let enc_idx = EncryptedIndex::new(cfg.datadir())?;
    dbg!(&enc_idx);

    let to_encrypt = index.difference_enc_idx(&enc_idx);
    dbg!(&to_encrypt);

    let dir_manager = Manager::new(&cfg.datadir());
    for entry in to_encrypt.iter() {
        dbg!(&entry);

        // if this is some, assume it's already encrypted and on-disk
        if entry.get_enc().is_some() {
            println!(
                "Skipping entry: {:?} ... because it's already encrypted.",
                entry
            );
            continue;
        }

        // read file data from entry and encrypt it . Need to read one of the paths
        let unenc_filedata = entry.read_filedata()?;
        let enc_filedata = bbox.encrypt(&unenc_filedata)?;

        let enc_mh = hash::hash(&enc_filedata);
        let enc_hash = enc_mh.to_bytes();
        let enc_path = dir_manager.write_encrypted(&enc_hash, &enc_filedata)?;
        dbg!(&enc_path);
    }

    Ok(())
}
