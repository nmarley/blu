//! Backup command - index paths, encrypt, and publish to the backend.

use crate::blob::BlobBuffer;
use crate::cli::clapargs::BackupArgs;
use crate::cli::helpers::{load_config_and_keys, push_indexes_or_fail, LoadOptions};
use crate::error::BluError;
use crate::hash::{self, Hash};
use itertools::Itertools;

/// Publish local files into the encrypted vault.
///
/// This command performs the following steps:
/// 1. Adds all files from the specified paths to the index
/// 2. Encrypts any chunks not yet encrypted
/// 3. Writes the updated indexes
/// 4. Merges remote indexes and pushes the catalog to the backend
///
/// It is idempotent and safe to run repeatedly.
pub async fn backup(args: BackupArgs) -> Result<(), BluError> {
    info!("Started backup");

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    let mut plain_index = cfg.load_plain_index_or_default(&keys);

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
    cfg.write_plain_index(&plain_index, &keys)?;

    // Step 2: Encrypt chunks that are not yet encrypted
    let mut blob_index = cfg.load_blob_index_or_default(&keys);
    let backend = match &args.backend {
        Some(name) => cfg.init_named_backend(name).await?,
        None => cfg.init_storage_backend().await?,
    };
    let mut blob_buf = BlobBuffer::new(&backend, keys.clone());

    let mut chunks_encrypted = 0;
    let files_map = plain_index.files_map_ref();
    let file_hashes = files_map.keys().clone().sorted_unstable();

    for file_hash in file_hashes {
        let file_ref = files_map
            .get(file_hash)
            .ok_or_else(|| BluError::FileHashNotFound {
                hash: file_hash.to_string(),
            })?;

        for cm in &file_ref.chunkmetas {
            if blob_index.has_chunk(&cm.hash) {
                continue; // Already encrypted
            }

            let block_ref = plain_index.blocks_map_ref().get(&cm.hash).ok_or_else(|| {
                BluError::BlockNotFound {
                    hash: cm.hash.to_string(),
                }
            })?;
            let data = plain_index.read_block_bytes(block_ref)?;

            // Verify hash matches
            let block_hash2 = Hash::from(hash::multihash(&data).to_bytes());
            assert_eq!(
                &cm.hash, &block_hash2,
                "block_hash mismatch (data corruption detected)"
            );

            blob_buf
                .add_chunk(&mut data.clone(), &mut blob_index)
                .await?;
            chunks_encrypted += 1;
        }
    }

    // Finalize and write blob index if we added chunks
    if chunks_encrypted > 0 || args.force {
        blob_buf.finalize(&mut blob_index).await?;
        cfg.write_blob_index(&blob_index, &keys)?;
    }

    // Publish indexes to the backend. The backend is the source of truth,
    // so this is not optional: the same backend that received the blobs
    // must also receive the updated indexes.
    push_indexes_or_fail(&cfg, &keys, args.backend.as_deref(), Some(&backend)).await?;

    // Print summary
    println!(
        "Backup complete: {} files indexed, {} chunks encrypted",
        files_added, chunks_encrypted
    );
    println!("Index contains {} files total", files_map.len());
    println!(
        "Blob index contains {} blob files",
        blob_index.count_blob_files()
    );

    Ok(())
}
