// #![allow(dead_code)]
// #![allow(unused_imports)]
// #![allow(unused_mut)]
// #![allow(unused_variables)]

use std::env;

use blu::age::BlackBox;
use blu::blob::BlobManager;
use blu::block::PlainIndex;
use blu::config;

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <dir-to-index>", args.next().unwrap());
        std::process::exit(1);
    }
    let dir = &args.nth(1).unwrap();

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);

    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;
    dbg!(&cfg);

    let index = PlainIndex::new(dir)?;
    // dbg!(&index);

    let mut blob_mgr = BlobManager::new(&cfg.datadir(), Some(&bbox));
    // dbg!(&blob_mgr);

    // Let the BlobManager determine if we need to encrypt something ...
    for (_block_hash, block_ref) in index.blocks_map_ref().iter() {
        // dbg!(&block_ref);
        let mut chunk_bytes = index.get_chunk_bytes(block_ref);
        blob_mgr.add_chunk(&mut chunk_bytes)?;
    }

    Ok(())
}
