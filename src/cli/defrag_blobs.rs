use std::collections::HashSet;
use std::path::PathBuf;

use crate::blob::{repack_blobs, rewrite_blobs};
use crate::cli::clapargs::DefragBlobsArgs;
use crate::cli::helpers::{load_config_and_keys, push_indexes_or_fail, LoadOptions};
use crate::error::BluError;
use crate::storage::BackendKind;
use crate::v3format;

/// Repack partially-dead blobs, or (with `--upgrade-format`) rewrite
/// all legacy v2 blobs into the v3 segmented format.
///
/// Loads the blob index from the vault config (like other commands)
/// and dispatches to the selected mode.
pub async fn defrag_blobs(args: DefragBlobsArgs) -> Result<(), BluError> {
    if args.upgrade_format {
        upgrade_format(args).await
    } else {
        repack(args).await
    }
}

/// Repack partially-dead blobs that have accumulated dead chunks from
/// prior delete operations. Reports candidates (dry-run) or performs
/// the repack and writes the updated index back.
async fn repack(args: DefragBlobsArgs) -> Result<(), BluError> {
    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;
    let mut blob_index = cfg.load_blob_index(&keys)?;

    let pending = blob_index.paths_to_repack.len();
    if pending == 0 {
        println!("No blobs need repacking");
        return Ok(());
    }

    println!("{} blob(s) queued for repack", pending);

    if args.dry_run {
        for blob_path in &blob_index.paths_to_repack {
            let live_chunks = blob_index
                .path_index
                .get(blob_path)
                .map(|s| s.len())
                .unwrap_or(0);
            println!("  {} ({} live chunks)", blob_path.display(), live_chunks,);
        }
        println!("(dry run, no changes made)");
        return Ok(());
    }

    let backend_name = args.backend.as_deref().unwrap_or(&cfg.default_backend);
    let backend = cfg.init_named_backend(backend_name).await?;

    let stats = repack_blobs(&mut blob_index, &backend, &keys).await?;

    cfg.write_blob_index(&blob_index, &keys)?;

    // Repacking rewrote blobs on the backend; sync the indexes so they
    // reflect the new blob layout.
    push_indexes_or_fail(&cfg, args.backend.as_deref(), Some(&backend)).await?;

    println!(
        "Repacked {} blob(s), moved {} chunks, deleted {} old blob(s)",
        stats.blobs_repacked, stats.chunks_moved, stats.old_blobs_deleted,
    );

    Ok(())
}

/// Rewrite every legacy v2 blob into the v3 segmented format.
///
/// Scans the distinct blob paths in the index, peeks each blob's
/// on-disk format version via a small header range read, and rewrites
/// the v2 ones through the shared repack machinery (which always emits
/// v3). Dry-run reports the count without touching the backend.
async fn upgrade_format(args: DefragBlobsArgs) -> Result<(), BluError> {
    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;
    let mut blob_index = cfg.load_blob_index(&keys)?;

    let backend_name = args.backend.as_deref().unwrap_or(&cfg.default_backend);
    let backend = cfg.init_named_backend(backend_name).await?;

    let v2_blobs = scan_v2_blobs(&blob_index, &backend).await?;

    if v2_blobs.is_empty() {
        println!("No v2 blobs to upgrade; all blobs are already v3");
        return Ok(());
    }

    println!("{} v2 blob(s) to upgrade to v3", v2_blobs.len());

    if args.dry_run {
        for blob_path in &v2_blobs {
            let chunks = blob_index
                .path_index
                .get(blob_path)
                .map(|s| s.len())
                .unwrap_or(0);
            println!("  {} ({} chunks)", blob_path.display(), chunks);
        }
        println!("(dry run, no changes made)");
        return Ok(());
    }

    let stats = rewrite_blobs(&mut blob_index, &backend, &keys, v2_blobs).await?;

    cfg.write_blob_index(&blob_index, &keys)?;

    // Upgrading rewrote blobs on the backend; sync the indexes so they
    // reflect the new v3 blob layout.
    push_indexes_or_fail(&cfg, args.backend.as_deref(), Some(&backend)).await?;

    println!(
        "Upgraded {} blob(s) to v3, moved {} chunks, deleted {} old blob(s)",
        stats.blobs_repacked, stats.chunks_moved, stats.old_blobs_deleted,
    );

    Ok(())
}

/// Return the set of distinct blob paths whose on-disk format is v2.
///
/// Reads only a small header prefix per blob to peek the version, so
/// this is cheap even for large backends.
async fn scan_v2_blobs(
    blob_index: &crate::blob::BlobIndex,
    backend: &BackendKind,
) -> Result<HashSet<PathBuf>, BluError> {
    let mut v2_blobs = HashSet::new();
    for blob_path in blob_index.path_index.keys() {
        let header = backend.read_range(blob_path, 0, 6).await?;
        if v3format::peek_version(&header) == Some(2) {
            v2_blobs.insert(blob_path.clone());
        }
    }
    Ok(v2_blobs)
}
