use std::env;

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
use blu::age::BlackBox;
use blu::block::{PlainBlockIndex, PlainFileIndex};
use blu::chunkfile::{CFAddStatus, ChunkFileIndex, ChunkFileManager, EncChunkLocation};
use blu::config;
// use blu::dir::Manager;
// use blu::hash::{self, Hash};
// use blu::metadata::{EncryptedIndex, Index};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <dir-to-index>", args.next().unwrap());
        std::process::exit(1);
    }
    let dir = &args.nth(1).unwrap();

    let _bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);

    let cfg = config::read_config(dir)?;
    dbg!(&cfg);

    let mut findex = PlainFileIndex::new(dir)?;
    dbg!(&findex);

    let mut bindex = PlainBlockIndex::new(&findex)?;
    dbg!(&bindex);

    let mut cfm = ChunkFileManager::new(&cfg.datadir());
    dbg!(&cfm);

    // enumerate the chunkfile ...?
    // need to somehow loop thru each chunk upon creation of the chunkfile
    //
    // now we have path...
    // for (enc hash, index) in chunkfile.positions {
    //     cfi.add_chunk_location(&enc_hash, &EncChunkLocation{
    //         path,
    //         index,
    //     });
    // }
    //
    // cfi.add_chunk_location(&enc_hash, &EncChunkLocation{
    //     path,
    //     index,
    // });
    // pub fn add_chunk_location(&mut self, chunk_hash: &Hash, location: &EncChunkLocation) {
    //     self.map.insert(chunk_hash.clone(), location.clone());
    // }

    // #[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
    // pub struct EncChunkLocation {
    //     path: PathBuf,
    //     index: usize,
    // }

    for (_file_hash, fileref) in findex.map_ref().iter() {
        // dbg!(&file_hash);
        // dbg!(&fileref);

        // iterate over plain chunks in file ...
        let fri = fileref.iter()?;
        for (count_chunk, plain_data_chunk) in fri.enumerate() {
            // 1. encrypt plain data chunk
            // 2. use cfm to add ...
            // 3. ... finalize cfm when done?
            dbg!(&hex::encode(&plain_data_chunk));

            // let enc_chunk = encrypt(plain_data_chunk);
            // match cfm.add_chunk(enc_chunk) {
            //     CFAddStatus::WrittenToDisk(path) => {
            //         // update path here ...
            //         bindex.update_encrypted(plain_chunk_hash, encrypted_hash);
            //     }
            //     CFAddStatus::AddedToMemory => {
            //         // do nothing ...
            //     }
            // };
            println!(
                "count_chunk = {} -------------------------------------------------------",
                count_chunk
            );
        }
        println!("========================================================================");
    }
    // TODO: update path in indexes
    let path = cfm.finalize();

    Ok(())
}
