#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_mut)]
#![allow(unused_variables)]

use std::env;

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
use blu::age::BlackBox;
use blu::block::PlainIndex;
use blu::chunkfile::{CFAddStatus, ChunkFileIndex, ChunkFileManager, EncChunkLocation};
use blu::config;

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

    let mut index = PlainIndex::new(dir)?;
    dbg!(&index);

    let mut cfm = ChunkFileManager::new(&cfg.datadir(), &bbox);
    dbg!(&cfm);

    // How to know if encryption is needed or not? It's possible the chunk is
    // already encrypted ...
    for (block_hash, block_ref) in index.blocks_map_ref().iter() {
        dbg!(&block_ref);
        if let Some(enc_hash) = &block_ref.encrypted_hash {
            println!(
                "block hash {:?} already encrypted with enc hash {:?} ...",
                block_hash, enc_hash
            );
        } else {
            // encrypt
            println!("encrypt this chunk!!");
            let dli = index.get_disk_location_index_for_blockref(block_ref);
            cfm.add_chunk_location(block_hash.clone(), dli)?;
        }
    }

    // for (_file_hash, fileref) in index.files_map_ref().iter() {
    //     // dbg!(&file_hash);
    //     dbg!(&fileref);
    //     // iterate over plain chunks in file ...
    //     let fri = fileref.iter()?;
    //     // for (count_chunk, plain_data_chunk) in fri.enumerate() {
    //     //     // 1. encrypt plain data chunk
    //     //     // 2. use cfm to add ...
    //     //     // 3. ... finalize cfm when done?
    //     //     dbg!(&hex::encode(&plain_data_chunk));
    //     //     let enc_chunk = bbox.encrypt(&plain_data_chunk)?;
    //     //     match cfm.add_chunk(&enc_chunk)? {
    //     //         CFAddStatus::WrittenToDisk(path) => {
    //     //             // update path here ...
    //     //             // index.update_encrypted(plain_chunk_hash, encrypted_hash);
    //     //             println!("Wrote chunkfile to disk at {}", path.display());
    //     //         }
    //     //         CFAddStatus::AddedToMemory => {
    //     //             // do nothing ...
    //     //             println!("Added to memory in active chunkfile");
    //     //         }
    //     //         CFAddStatus::NothingToDo => {
    //     //             // do nothing ...
    //     //             println!("Nothing to do");
    //     //         }
    //     //     };
    //     //     println!(
    //     //         "count_chunk = {} -------------------------------------------------------",
    //     //         count_chunk
    //     //     );
    //     // }
    //     println!("========================================================================");
    // }
    // TODO: update path in indexes
    // let path = cfm.finalize();

    Ok(())
}
