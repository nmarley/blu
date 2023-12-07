use itertools::Itertools;
use std::path::Path;

use crate::age::BlackBox;
use crate::blob::BlobBuffer;
use crate::cli::clapargs::EncryptFilesArgs;
use crate::config;
use crate::hash::{self, Hash};

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// Encrypt the plain text files in the index
pub fn encrypt_files(args: EncryptFilesArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Started encrypt_files util");

    let dir = Path::new(".");

    info!("force_write_index option: {}", args.force_write_index);

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);

    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;
    // dbg!(&cfg);

    let plain_index = cfg.load_plain_index(&bbox).unwrap();

    // TODO: ... do we only encrypt the files in index, or do we add/update
    // files, THEN encrypt everything that is not already encrypted?

    let mut blob_index = cfg.load_blob_index(&bbox).unwrap_or_default();
    info!(
        "Blob index has {} blob files",
        blob_index.count_blob_files()
    );

    let backend = cfg.init_storage_backend()?;

    // NOTE:
    //     `*` derefs the `Box<dyn StorageBackend>`
    //     BlobBuffer::new expects a `&dyn StorageBackend`
    let mut blob_buf = BlobBuffer::new(&(*backend), bbox.clone());

    // need some kind of selection mechanism here -- which files to encrypt?
    // for now, we encrypt them all and sort the selection out later
    let mut count_added = 0;
    info!("iterating plain_index now");

    // Start with files, random blocks will make the data storage less organized
    // and more scattered on disk. Async threads won't even help with bad
    // design.

    let files_map = plain_index.files_map_ref();
    let file_hashes = files_map.keys().clone().sorted_unstable();
    // let mut file_hashes: Vec<Hash> = files_map.keys().clone().collect();
    // file_hashes.sort_unstable();

    for file_hash in file_hashes {
        info!("file_hash: {:?}", &file_hash.dbg_short(7));
        let file_ref = files_map.get(file_hash).unwrap();
        info!("chunks count: {}", file_ref.chunkmetas.len());

        for (count, cm) in file_ref.chunkmetas.iter().enumerate() {
            info!("\t chunkmeta[{}]: {:?}", count, cm.hash.dbg_short(7));
            if blob_index.has_chunk(&cm.hash) {
                info!("already encrypted, moving on...");
                continue;
            }

            let block_ref = plain_index.blocks_map_ref().get(&cm.hash).unwrap();
            let data = plain_index.read_block_bytes(block_ref);

            // NOTE: we probably want to somehow keep this around / add it as a
            // checksum to ensure that the data is not corrupted
            let block_hash2 = Hash::from(hash::multihash(&data).to_bytes());
            assert_eq!(
                &cm.hash, &block_hash2,
                "block_hash mismatch (unresolvable black death of the universe error)"
            );

            // add it to the blob buffer
            // info!("Adding chunk to blob buffer");
            blob_buf.add_chunk(&mut data.clone(), &mut blob_index)?;
            count_added += 1;
        }
    }

    println!("Added {} new chunks to blob buffer", count_added);
    if count_added > 0 || args.force_write_index {
        match blob_buf.finalize(&mut blob_index) {
            Ok(_) => println!("Finalized blob buffer!"),
            Err(e) => println!("Error finalizing blob buffer: {}", e),
        }
        match cfg.write_blob_index(&blob_index, &bbox) {
            Ok(_) => println!("Wrote blob index!"),
            Err(e) => println!("Error writing blob index: {}", e),
        }
    }

    Ok(())
}
