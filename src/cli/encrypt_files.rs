use itertools::Itertools;

use crate::blob::BlobBuffer;
use crate::cli::clapargs::EncryptFilesArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::error::BluError;
use crate::hash::{self, Hash};

/// Encrypt the plain text files in the index
pub async fn encrypt_files(args: EncryptFilesArgs) -> Result<(), BluError> {
    info!("Started encrypt_files util");
    info!("force_write_index option: {}", args.force_write_index);

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    let plain_index = cfg.load_plain_index(&keys)?;

    // TODO: ... do we only encrypt the files in index, or do we add/update
    // files, THEN encrypt everything that is not already encrypted?

    let mut blob_index = match cfg.load_blob_index(&keys) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => Default::default(),
        Err(e) => return Err(e),
    };
    info!(
        "Blob index has {} blob files",
        blob_index.count_blob_files()
    );

    let backend = cfg.init_storage_backend().await?;
    let mut blob_buf = BlobBuffer::new(&backend, keys.clone());

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

    // TODO: consider rayon for parallelizing this
    for file_hash in file_hashes {
        info!("file_hash: {:?}", &file_hash.dbg_short(7));
        let file_ref = files_map
            .get(file_hash)
            .ok_or_else(|| BluError::FileHashNotFound {
                hash: file_hash.to_string(),
            })?;
        info!("chunks count: {}", file_ref.chunkmetas.len());

        // TODO: possible to do nested parallel iteration?
        for (count, cm) in file_ref.chunkmetas.iter().enumerate() {
            info!("\t chunkmeta[{}]: {:?}", count, cm.hash.dbg_short(7));
            if blob_index.has_chunk(&cm.hash) {
                info!("already encrypted, moving on...");
                continue;
            }

            let block_ref = plain_index.blocks_map_ref().get(&cm.hash).ok_or_else(|| {
                BluError::BlockNotFound {
                    hash: cm.hash.to_string(),
                }
            })?;
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
            blob_buf
                .add_chunk(&mut data.clone(), &mut blob_index)
                .await?;
            count_added += 1;
        }
    }

    println!("Added {} new chunks to blob buffer", count_added);
    if count_added > 0 || args.force_write_index {
        match blob_buf.finalize(&mut blob_index).await {
            Ok(_) => println!("Finalized blob buffer!"),
            Err(e) => println!("Error finalizing blob buffer: {}", e),
        }
        match cfg.write_blob_index(&blob_index, &keys) {
            Ok(_) => println!("Wrote blob index!"),
            Err(e) => println!("Error writing blob index: {}", e),
        }
    }

    Ok(())
}
