use clap::Parser;
use std::fs;
use std::io::Write;
use std::path::Path;

use blu::age::BlackBox;
use blu::block::PlainIndex;

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";

#[derive(Parser)]
pub struct Args {
    pub dir: String,
    pub outfile: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let dir = args.dir;

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let index = PlainIndex::new(dir)?;
    // dbg!(&index);

    let outfile = args.outfile;
    // writing index for testing
    write_index_file(&index, &bbox, &outfile)?;

    Ok(())
}

fn write_index_file<P: AsRef<Path>>(
    index: &PlainIndex,
    bbox: &BlackBox,
    outfile: P,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut enc_idx_bytes = Vec::new();
    index.write(&mut enc_idx_bytes, bbox)?;
    let mut file = fs::File::create(outfile)?;
    file.write_all(&enc_idx_bytes)?;
    Ok(())
}
