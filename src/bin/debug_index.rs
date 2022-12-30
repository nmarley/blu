use std::env;
use std::path::Path;

use blu::block::PlainIndex;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <dir-to-index>", args.next().unwrap());
        std::process::exit(1);
    }
    let index_dir = &args.nth(1).unwrap();

    let mut index = PlainIndex::new(index_dir)?;
    dbg!(&index);
    println!("uniq bytes indexed: {}", index.uniq_bytes_indexed());
    println!("total bytes indexed: {}", index.total_bytes_indexed());

    let old_filename = Path::new(index_dir).join("hi.txt");
    let new_filename = Path::new(index_dir).join("hello.txt");
    // rename to test
    std::fs::rename(&old_filename, &new_filename)?;

    let tuples = index.update(index_dir)?;
    dbg!(&index);
    println!("uniq bytes indexed: {}", index.uniq_bytes_indexed());
    println!("total bytes indexed: {}", index.total_bytes_indexed());

    dbg!(&tuples);
    // move it back
    std::fs::rename(&new_filename, &old_filename)?;

    Ok(())
}
