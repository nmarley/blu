//! Sync command - combines add + encrypt in a single operation.

use crate::blob::BlobBuffer;
use crate::block::PlainIndex;
use crate::cli::clapargs::SyncArgs;
use crate::cli::helpers::{load_config_and_blackbox, LoadOptions};
use crate::error::BluError;
use crate::hash::{self, Hash};
use itertools::Itertools;

/// Sync local files to the encrypted backend.
///
/// This command performs the following steps:
/// 1. Adds all files from the specified paths to the index
/// 2. Encrypts any chunks not yet encrypted
/// 3. Writes the updated indexes
///
/// It is idempotent and safe to run repeatedly.
pub fn sync(args: SyncArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Started sync");

    let (cfg, bbox) = load_config_and_blackbox(&LoadOptions::default())?;

    // Load the plain index (or create a new one if none exists)
    let mut plain_index = match cfg.load_plain_index(&bbox) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => {
            info!("No existing index, creating new one");
            PlainIndex::new_empty()
        }
        Err(e) => return Err(e.into()),
    };

    // Determine paths to add
    let paths_to_add = if args.paths.is_empty() {
        vec![".".to_string()]
    } else {
        args.paths.clone()
    };

    // Step 1: Add files to the index
    let mut files_added = 0;
    for p in &paths_to_add {
        info!("Adding {:?}", p);
        let before_count = plain_index.files_map_ref().len();
        plain_index.add(p.clone(), None)?;
        let after_count = plain_index.files_map_ref().len();
        files_added += after_count.saturating_sub(before_count);
    }

    // Write the updated plain index
    cfg.write_plain_index(&plain_index, &bbox)?;

    // Step 2: Encrypt chunks that are not yet encrypted
    let mut blob_index = match cfg.load_blob_index(&bbox) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => Default::default(),
        Err(e) => return Err(e.into()),
    };
    let backend = cfg.init_storage_backend()?;
    let mut blob_buf = BlobBuffer::new(&(*backend), bbox.clone());

    let mut chunks_encrypted = 0;
    let files_map = plain_index.files_map_ref();
    let file_hashes = files_map.keys().clone().sorted_unstable();

    for file_hash in file_hashes {
        let file_ref = files_map.get(file_hash).unwrap();

        for cm in &file_ref.chunkmetas {
            if blob_index.has_chunk(&cm.hash) {
                continue; // Already encrypted
            }

            let block_ref = plain_index.blocks_map_ref().get(&cm.hash).unwrap();
            let data = plain_index.read_block_bytes(block_ref);

            // Verify hash matches
            let block_hash2 = Hash::from(hash::multihash(&data).to_bytes());
            assert_eq!(
                &cm.hash, &block_hash2,
                "block_hash mismatch (data corruption detected)"
            );

            blob_buf.add_chunk(&mut data.clone(), &mut blob_index)?;
            chunks_encrypted += 1;
        }
    }

    // Finalize and write blob index if we added chunks
    if chunks_encrypted > 0 || args.force {
        blob_buf.finalize(&mut blob_index)?;
        cfg.write_blob_index(&blob_index, &bbox)?;
    }

    // Push indexes to remote if requested
    if args.push {
        println!("Pushing indexes to remote backend...");
        cfg.push_indexes(&*backend)?;
        println!("Indexes pushed successfully");
    }

    // Print summary
    println!(
        "Sync complete: {} files indexed, {} chunks encrypted",
        files_added, chunks_encrypted
    );

    if args.verbose {
        println!("Index contains {} files total", files_map.len());
        println!(
            "Blob index contains {} blob files",
            blob_index.count_blob_files()
        );
    }

    Ok(())
}
