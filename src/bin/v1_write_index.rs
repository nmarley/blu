use clap::Parser;
use std::fs;
use std::io::Write;
use std::path::Path;

use blu::age::BlackBox;
use blu::config;
use blu::metadata::{EncryptedIndex, Index};

#[derive(Parser)]
pub struct Args {
    pub dir: String,
    pub outfile: String,
}

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");
const V1_WARNING: &str = "WARNING: This tool is from v0.1 and will not work w/current codebase";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n{}\n", V1_WARNING);

    let args = Args::parse();
    let dir = match args.dir {
        dir if dir.starts_with("./") => dir.strip_prefix("./").unwrap().to_string(),
        dir => dir,
    };

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let mut index = Index::new(&dir)?;

    let cfg = config::read_config(&dir)?;
    dbg!(&cfg);

    let enc_idx = EncryptedIndex::new(cfg.datadir())?;
    dbg!(&enc_idx);

    let to_restore = enc_idx.difference_idx(&mut index, Some(&bbox));
    dbg!(&to_restore);

    // dbg!(&index);

    let outfile = args.outfile;
    // writing index for testing
    write_index_file(&index, &bbox, &outfile)?;

    Ok(())
}

fn write_index_file<P: AsRef<Path>>(
    index: &Index,
    bbox: &BlackBox,
    outfile: P,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut enc_idx_bytes = Vec::new();
    index.write(&mut enc_idx_bytes, bbox)?;
    let mut file = fs::File::create(outfile)?;
    file.write_all(&enc_idx_bytes)?;
    Ok(())
}
