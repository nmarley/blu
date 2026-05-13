//! Pull command - download indexes from remote backend.

use crate::cli::clapargs::PullArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};

/// Pull indexes from the remote backend.
///
/// This downloads the encrypted index files from the backend,
/// allowing access to the vault from a different machine.
pub async fn pull(args: PullArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Started pull");

    let (cfg, _keys) = load_config_and_keys(&LoadOptions::default())?;

    // Check if local indexes exist and warn if not using --force
    let plain_index_path = cfg.idxdir().join("index.dat");
    let blob_index_path = cfg.idxdir().join("blob_index.dat");

    if !args.force && (plain_index_path.exists() || blob_index_path.exists()) {
        eprintln!("Warning: Local indexes exist and will be overwritten.");
        eprintln!("Use --force to confirm, or back up your local indexes first.");
        return Err("Local indexes exist. Use --force to overwrite.".into());
    }

    let backend = match &args.backend {
        Some(name) => cfg.init_named_backend(name).await?,
        None => cfg.init_storage_backend().await?,
    };

    println!("Pulling indexes from remote backend...");
    cfg.pull_indexes(&backend).await?;
    println!("Indexes pulled successfully");

    Ok(())
}
