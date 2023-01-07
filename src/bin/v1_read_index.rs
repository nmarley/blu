use std::{env, fs};

use blu::age::BlackBox;
use blu::io::BlackBoxSerializable;
use blu::metadata::Index;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");
const V1_WARNING: &str = "WARNING: This tool is from v0.1 and will not work w/current codebase";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n{}\n", V1_WARNING);

    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <index-file>", args.next().unwrap());
        std::process::exit(1);
    }
    let index_file = &args.nth(1).unwrap();

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let data = fs::read(index_file)?;
    let index = Index::read(&data[..], &bbox)?;
    dbg!(&index);

    Ok(())
}
