use std::io::Write;
use std::{env, fs};

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
use blu::age::BlackBox;
use blu::config;
use blu::metadata::{EncryptedIndex, Index};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <dir-to-index>", args.next().unwrap());
        std::process::exit(1);
    }
    let dir = &args.nth(1).unwrap();

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let mut index = Index::new(dir)?;

    let cfg = config::read_config(dir)?;
    dbg!(&cfg);

    let enc_idx = EncryptedIndex::new(cfg.datadir())?;
    dbg!(&enc_idx);

    let to_restore = enc_idx.difference_idx(&mut index, Some(&bbox));
    dbg!(&to_restore);

    // dbg!(&index);

    // writing index for testing
    write_index_file(&index, &bbox)?;

    Ok(())
}

fn write_index_file(index: &Index, bbox: &BlackBox) -> Result<(), Box<dyn std::error::Error>> {
    let mut enc_idx_bytes = Vec::new();
    index.write(&mut enc_idx_bytes, bbox)?;
    let mut file = fs::File::create("test-idx-enc.dat")?;
    file.write_all(&enc_idx_bytes)?;
    Ok(())
}
