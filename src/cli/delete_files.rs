use std::collections::HashSet;

use crate::blob::repack_blobs;
use crate::cli::clapargs::DeleteFilesArgs;
use crate::cli::helpers::{load_config_and_keys, push_indexes_or_fail, LoadOptions};
use crate::error::BluError;
use crate::format::human_bytes;
use crate::hash::Hash;

/// Delete files from the plain index and cascade through the full
/// data pipeline: plain index, blocks, blob index, tags, and backend
/// storage.
///
/// When all chunks in a blob file are removed, the blob is deleted
/// from the configured storage backend. Partially-dead blobs (still
/// containing live chunks) are left for defrag to repack later.
pub async fn delete_files(args: DeleteFilesArgs) -> Result<(), BluError> {
    if args.filter.is_none() && !args.all {
        return Err(BluError::Internal("must specify --filter or --all".into()));
    }

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;
    let mut plain_index = cfg.load_plain_index(&keys)?;
    let mut tag_index = cfg.load_tag_index_or_default(&keys);
    let mut blob_index = cfg.load_blob_index_or_default(&keys);

    // Collect file hashes that match the filter
    let hashes_to_delete =
        collect_matching_hashes(&plain_index, &tag_index, args.filter.as_deref(), args.all);

    if hashes_to_delete.is_empty() {
        println!("No files matched the specified criteria");
        return Ok(());
    }

    // Display what will be deleted
    let mut total_bytes: u64 = 0;
    let mut total_chunks: usize = 0;
    for file_hash in &hashes_to_delete {
        if let Some(file_ref) = plain_index.get_fileref_ref(file_hash) {
            let size = file_ref.total_size();
            total_bytes += size;
            total_chunks += file_ref.chunkmetas.len();

            let mut paths: Vec<_> = file_ref.paths.iter().collect();
            paths.sort_unstable();

            println!(
                "  {} ({}, {} chunks)",
                file_hash.dbg_short(15),
                human_bytes(size),
                file_ref.chunkmetas.len(),
            );
            for p in &paths {
                println!("    {}", p.display());
            }

            let tags = tag_index.get_tags(file_hash);
            if !tags.is_empty() {
                println!("    tags: {}", tags.join(", "));
            }
        }
    }

    println!(
        "\n{} file(s), {} total, {} chunks",
        hashes_to_delete.len(),
        human_bytes(total_bytes),
        total_chunks,
    );

    if args.dry_run {
        println!("(dry run, no changes made)");
        return Ok(());
    }

    // Perform the deletion cascade
    let mut blocks_removed: usize = 0;
    let mut chunks_deleted: usize = 0;

    for file_hash in &hashes_to_delete {
        // Get the chunk hashes before removing the file entry
        let chunk_hashes: Vec<Hash> = match plain_index.get_fileref_ref(file_hash) {
            Some(file_ref) => file_ref
                .chunkmetas
                .iter()
                .map(|cm| cm.hash.clone())
                .collect(),
            None => continue,
        };

        // Remove file from plain index
        plain_index.files.remove(file_hash);

        // For each chunk, remove this file's reference from the block
        for chunk_hash in &chunk_hashes {
            let block_unreferenced = match plain_index.blocks.get_mut(chunk_hash) {
                Some(block_ref) => block_ref.delete_fileref(file_hash),
                None => false,
            };

            if block_unreferenced {
                // Block has no remaining references; remove it
                plain_index.blocks.remove(chunk_hash);
                blocks_removed += 1;

                // Remove chunk from blob index (if encrypted)
                if blob_index.has_chunk(chunk_hash) {
                    blob_index.delete_chunk(chunk_hash)?;
                    chunks_deleted += 1;
                }
            }
        }

        // Remove all tags for this file
        tag_index.drop_all_tags(file_hash);
    }

    // Delete fully-dead blobs from the storage backend
    let dead_blob_paths = blob_index.drain_paths_to_delete();
    let blobs_deleted = dead_blob_paths.len();

    // Initialize backend if we need to delete blobs or scrub
    let need_backend =
        !dead_blob_paths.is_empty() || (args.scrub && !blob_index.paths_to_repack.is_empty());
    let backend = if need_backend {
        let backend_name = args.backend.as_deref().unwrap_or(&cfg.default_backend);
        Some(cfg.init_named_backend(backend_name).await?)
    } else {
        None
    };

    if !dead_blob_paths.is_empty() {
        let backend = backend.as_ref().expect("backend initialized above");
        let mut delete_errors: Vec<String> = Vec::new();

        for blob_path in &dead_blob_paths {
            if let Err(e) = backend.delete(blob_path).await {
                delete_errors.push(format!("{}: {}", blob_path.display(), e));
            }
        }

        if !delete_errors.is_empty() {
            eprintln!(
                "Warning: failed to delete {} blob(s) from backend:",
                delete_errors.len()
            );
            for msg in &delete_errors {
                eprintln!("  {}", msg);
            }
        }
    }

    // Scrub: repack partially-dead blobs inline
    let repack_stats = if args.scrub && !blob_index.paths_to_repack.is_empty() {
        let backend = backend.as_ref().expect("backend initialized above");
        Some(repack_blobs(&mut blob_index, backend, &keys).await?)
    } else {
        None
    };

    // Advisory: tell the user about pending repack candidates
    let pending_repack = blob_index.paths_to_repack.len();
    if !args.scrub && pending_repack > 0 {
        println!(
            "\n{} blob(s) have dead chunks and can be reclaimed with \
             --scrub or defrag-blobs",
            pending_repack,
        );
    }

    // Write all modified indexes back
    cfg.write_plain_index(&plain_index, &keys)?;
    cfg.write_tag_index(&tag_index, &keys)?;
    cfg.write_blob_index(&blob_index, &keys)?;

    // Sync the indexes to the backend. Reuse the backend handle if one
    // was initialized for blob deletion; otherwise the helper resolves
    // it by name.
    push_indexes_or_fail(&cfg, &keys, args.backend.as_deref(), backend.as_ref()).await?;

    println!(
        "Deleted {} file(s), removed {} unreferenced blocks, \
         {} blob chunks removed, {} blob(s) deleted from backend",
        hashes_to_delete.len(),
        blocks_removed,
        chunks_deleted,
        blobs_deleted,
    );

    if let Some(stats) = repack_stats {
        println!(
            "Scrub: repacked {} blob(s), moved {} chunks, \
             deleted {} old blob(s)",
            stats.blobs_repacked, stats.chunks_moved, stats.old_blobs_deleted,
        );
    }

    Ok(())
}

/// Collect file hashes from the plain index that match the given filter.
///
/// Matches against hash hex string, path substrings (case-insensitive),
/// and tags (case-insensitive). Returns all file hashes if `all` is true.
fn collect_matching_hashes(
    plain_index: &crate::block::PlainIndex,
    tag_index: &crate::tag::TagIndex,
    filter: Option<&str>,
    all: bool,
) -> HashSet<Hash> {
    let files_ref = plain_index.files_map_ref();
    let mut matched: HashSet<Hash> = HashSet::new();

    for (file_hash, file_ref) in files_ref.iter() {
        if all {
            matched.insert(file_hash.clone());
            continue;
        }

        let filter = match filter {
            Some(f) => f,
            None => continue,
        };

        // Match by hash hex substring
        if file_hash.to_string().contains(filter) {
            matched.insert(file_hash.clone());
            continue;
        }

        // Match by path substring (case-insensitive)
        let filter_lower = filter.to_lowercase();
        if file_ref
            .paths
            .iter()
            .any(|p| p.to_string_lossy().to_lowercase().contains(&filter_lower))
        {
            matched.insert(file_hash.clone());
            continue;
        }

        // Match by tag substring (case-insensitive)
        if tag_index
            .get_tags(file_hash)
            .iter()
            .any(|t| t.to_lowercase().contains(&filter_lower))
        {
            matched.insert(file_hash.clone());
        }
    }

    matched
}
