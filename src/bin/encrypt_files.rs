use std::env;

use blu::age::BlackBox;
use blu::blob::BlobBuffer;
use blu::config;
use blu::hash::{self, Hash};

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

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
    // dbg!(&cfg);

    let _datadir = cfg.datadir();
    // dbg!(&datadir);

    let plain_index = cfg.load_plain_index(&bbox).unwrap();
    // dbg!(&plain_index);

    // TODO: ... do we only encrypt the files in index, or do we add/update
    // files, THEN encrypt everything that is not already encrypted?

    let mut blob_index = cfg.load_blob_index(&bbox).unwrap_or_default();
    let mut blob_buf = BlobBuffer::new(cfg.datadir(), bbox.clone());
    dbg!(&blob_buf);

    // TODO: determine whether to use underscore in e.g. block_hash or
    // blockhash, block_ref or blockref, and then stay consistent thru the
    // codebase.

    // need some kind of selection mechanism here -- which files to encrypt?
    // for now, we encrypt them all and sort the selection out later
    for (blockhash, blockref) in plain_index.blocks_map_ref() {
        // let block = index.get_block(blockref).unwrap();
        // dbg!(&blockhash);
        if blob_index.has_chunk(blockhash) {
            println!("already encrypted: {:?}", blockhash);
            continue;
        }

        println!("NOT encrypted: {:?}, adding ...", blockhash);
        let data = plain_index.read_block_bytes(blockref);
        // println!("data: {:?}", data);

        // NOTE: we probably want to somehow keep this around / add it as a
        // checksum to ensure that the data is not corrupted
        let block_hash2 = Hash::from(hash::multihash(&data).to_bytes());
        assert_eq!(
            blockhash, &block_hash2,
            "blockhash mismatch (unresolvable black death of the universe error)"
        );

        // add it to the blob buffer
        blob_buf.add_chunk(&mut data.clone(), &mut blob_index)?;
        println!("========================================================================");
    }
    match blob_buf.finalize(&mut blob_index) {
        Ok(_) => println!("Finalized blob buffer!"),
        Err(e) => println!("Error finalizing blob buffer: {}", e),
    }
    match cfg.write_blob_index(&blob_index, &bbox) {
        Ok(_) => println!("Wrote blob index!"),
        Err(e) => println!("Error writing blob index: {}", e),
    }

    Ok(())
}
