use std::env;

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
use blu::age::BlackBox;
use blu::block::{PlainBlockIndex, PlainFileIndex};
use blu::chunkfile::ChunkFileManager;
use blu::config;
use blu::dir::Manager;
use blu::hash::{self, Hash};
use blu::metadata::{EncryptedIndex, Index};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <dir-to-index>", args.next().unwrap());
        std::process::exit(1);
    }
    let dir = &args.nth(1).unwrap();

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);

    let cfg = config::read_config(dir)?;
    dbg!(&cfg);

    let mut findex = PlainFileIndex::new(dir)?;
    dbg!(&findex);

    let mut bindex = PlainBlockIndex::new(&findex)?;
    dbg!(&bindex);

    let mut cfm = ChunkFileManager::new(&cfg.datadir());
    dbg!(&cfm);

    for (file_hash, fileref) in findex.map_ref().iter() {
        dbg!(&file_hash);
        dbg!(&fileref);

        // iterate over plain chunks in file ...
        let mut fri = fileref.iter()?;
        let mut count_chunk = 0;
        for plain_data_chunk in fri {
            dbg!(&plain_data_chunk);
            println!(
                "count_chunk = {} -------------------------------------------------------",
                count_chunk
            );
            count_chunk += 1;
        }
        println!("========================================================================");
    }

    Ok(())
}
