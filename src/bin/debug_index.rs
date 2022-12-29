use std::env;

use blu::block::PlainIndex;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <dir-to-index>", args.next().unwrap());
        std::process::exit(1);
    }
    let index_dir = &args.nth(1).unwrap();

    let index = PlainIndex::new(index_dir).unwrap();
    dbg!(&index);

    Ok(())
}
