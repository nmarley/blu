use std::{env, fs};

use blu::age::BlackBox;
use blu::block::PlainIndex;

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <index-file>", args.next().unwrap());
        std::process::exit(1);
    }
    let index_file = &args.nth(1).unwrap();

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let data = fs::read(index_file)?;
    let index = PlainIndex::read(&data[..], &bbox)?;
    dbg!(&index);

    Ok(())
}
