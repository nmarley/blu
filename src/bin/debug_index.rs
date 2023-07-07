#![allow(clippy::uninlined_format_args)]

use std::env;
use std::path::Path;

use blu::block::PlainIndex;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <dir-to-index>", args.next().unwrap());
        std::process::exit(1);
    }

    // move into the basedir for all operations, like `git -C <dir>`
    let basedir = &args.nth(1).unwrap();
    env::set_current_dir(basedir)?;
    let index_dir = Path::new(".");

    let index = PlainIndex::new(index_dir)?;
    dbg!(&index);
    println!("uniq bytes indexed: {}", index.uniq_bytes_indexed());
    println!("total bytes indexed: {}", index.total_bytes_indexed());
    println!("dupe bytes indexed: {}", index.duplicate_bytes_indexed());

    Ok(())
}
