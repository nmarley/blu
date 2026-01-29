use std::fs;
use std::path::Path;

use crate::age::BlackBox;
use crate::blob::BlobIndex;
use crate::cli::clapargs::DefragBlobsArgs;
use crate::cli::helpers::{load_config_and_blackbox, LoadOptions};
use crate::io::BlackBoxSerializable;

/// Defrag blobs is still a WIP
pub fn defrag_blobs(args: DefragBlobsArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Started defrag_blobs util");

    info!("blob_index_path: {}", args.blob_index_path);

    let (_cfg, bbox) = load_config_and_blackbox(&LoadOptions::default())?;

    let blob_index = load_blob_index(&bbox, args.blob_index_path).unwrap();
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
            blob_size += blob_index.map.get(chunk_hash).unwrap().position.size;
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

fn load_blob_index<P: AsRef<Path>>(bbox: &BlackBox, index_path: P) -> Option<BlobIndex> {
    // read index file data or return None
    let index_data: Vec<u8> = fs::read(index_path.as_ref()).ok()?;
    // deserialize + decompress + decrypt index or return None
    BlobIndex::read(&index_data[..], bbox).ok()
}
