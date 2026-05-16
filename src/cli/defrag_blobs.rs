use crate::blob::repack_blobs;
use crate::cli::clapargs::DefragBlobsArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::error::BluError;

/// Repack partially-dead blobs that have accumulated dead chunks
/// from prior delete operations.
///
/// Loads the blob index from the vault config (like other commands),
/// checks `paths_to_repack` for candidates, and either reports what
/// would be done (dry-run) or performs the repack and writes the
/// updated index back.
pub async fn defrag_blobs(args: DefragBlobsArgs) -> Result<(), BluError> {
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

    println!(
        "Repacked {} blob(s), moved {} chunks, deleted {} old blob(s)",
        stats.blobs_repacked, stats.chunks_moved, stats.old_blobs_deleted,
    );

    Ok(())
}
