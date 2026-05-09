use itertools::Itertools;
use std::path::PathBuf;

use crate::cli::clapargs::DeleteFilesArgs;
use crate::cli::helpers::{load_config_and_blackbox, LoadOptions};
use crate::error::BluError;

// TODO: delete by hash (ONLY -- API will be built around this and search can
// be used for getting hashesfrom files)

/// Delete data from index and mark associated encrypted blobs as deleted.
pub fn delete_files(args: DeleteFilesArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (cfg, bbox) = load_config_and_blackbox(&LoadOptions::default())?;
    let plain_index = cfg.load_plain_index(&bbox)?;
    let tag_index = match cfg.load_tag_index(&bbox) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => Default::default(),
        Err(e) => return Err(e.into()),
    };

    // TODO: maybe add this (sorted file hashes) to index API and add the test there?
    let files_ref = plain_index.files_map_ref();
    let file_hashes = files_ref.keys().sorted_unstable();

    if args.dry_run {
        info!("Got dry_run flag -- will not delete");
    }

    // per hash file hash, list the data
    for file_hash in file_hashes {
        let file_ref = files_ref.get(file_hash).unwrap();
        if let Some(ref filter) = args.filter {
            let mut found_match = false;
            // try and filter
            if file_hash.to_string().contains(filter) {
                found_match = true;
                println!("Got a hash match!");
            }

            if !found_match {
                continue;
            };
        };

        let file_size = file_ref.total_size();
        let chunkmetas = &file_ref.chunkmetas;

        println!("  Hash: {}", file_hash.dbg_short(15));
        println!("  Size: {}", file_size);
        println!("Chunks: {}", chunkmetas.len());

        // TODO: anything here? It should be removed from PlainIndex as well, yeah?
        let mut paths: Vec<&PathBuf> = file_ref.paths.iter().collect();
        paths.sort_unstable();

        // TODO: what if all tag references removed?
        let _tags = tag_index.get_tags(file_hash);
    }

    Ok(())
}
