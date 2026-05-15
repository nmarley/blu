use std::fs;
use std::path::Path;

use crate::blob::BlobIndex;
use crate::cli::clapargs::DefragBlobsArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::dek_provider::DekProvider;
use crate::error::BluError;
use crate::io::EncryptedSerializable;

/// Defrag blobs is still a WIP
pub fn defrag_blobs(args: DefragBlobsArgs) -> Result<(), BluError> {
    info!("Started defrag_blobs util");

    info!("blob_index_path: {}", args.blob_index_path);

    let (_cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    let blob_index = load_blob_index(&keys, args.blob_index_path)?;
    info!(
        "Blob index has {} blob files",
        blob_index.count_blob_files()
    );

    // Now we've hit the classic Bin Packing problem:
    // https://en.wikipedia.org/wiki/Bin_packing_problem
    //
    // Let's just use the First-Fit Decreasing (FFD) algorithm and call it good
    // (enough).

    // let backend = cfg.init_storage_backend()?;

    for (blob_path, set_chunk_hashes) in blob_index.path_index.iter() {
        let mut blob_size = 0_usize;
        for chunk_hash in set_chunk_hashes.iter() {
            let chunk_entry =
                blob_index
                    .map
                    .get(chunk_hash)
                    .ok_or_else(|| BluError::BlockNotFound {
                        hash: chunk_hash.to_string(),
                    })?;
            blob_size += chunk_entry.position.size;
        }
        info!(
            "Got blob path: {} with {} hashes, {} total bytes",
            blob_path.display(),
            set_chunk_hashes.len(),
            blob_size,
        );
    }

    if !args.dry_run {
        info!("do something here");
    } else {
        info!("Got dry_run flag, will not write");
    }

    Ok(())
}

fn load_blob_index<P: AsRef<Path>>(
    keys: &DekProvider,
    index_path: P,
) -> Result<BlobIndex, BluError> {
    let path = index_path.as_ref();
    let index_data: Vec<u8> = fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => BluError::IndexNotFound(path.display().to_string()),
        _ => BluError::Internal(format!("failed to read index at {}: {}", path.display(), e)),
    })?;
    BlobIndex::read(&index_data[..], keys).map_err(|e| BluError::IndexLoadFailed {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}
