use std::{env, fs};

use blu::age::BlackBox;
use blu::blob::BlobIndex;
use blu::io::BlackBoxSerializable;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <index-file>", args.next().unwrap());
        std::process::exit(1);
    }
    let index_file = &args.nth(1).unwrap();

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let data = fs::read(index_file)?;
    let index = BlobIndex::read(&data[..], &bbox)?;
    dbg!(&index);

    Ok(())
}
