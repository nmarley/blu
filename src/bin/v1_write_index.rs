use std::io::Write;
use std::{env, fs};

use blu::age::BlackBox;
use blu::config;
use blu::metadata::{EncryptedIndex, Index};

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");
const V1_WARNING: &str = "WARNING: This tool is from v0.1 and will not work w/current codebase";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n{}\n", V1_WARNING);

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
