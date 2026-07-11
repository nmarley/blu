//! Pull command - download indexes from remote backend.

use crate::cli::clapargs::PullArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::error::BluError;

/// Pull indexes from the remote backend.
///
/// Default: fetch remote indexes and union-merge into local (keeps
/// local-only entries). `--force` discards local indexes and takes the
/// remote copy only (hard reset).
pub async fn pull(args: PullArgs) -> Result<(), BluError> {
    info!("Started pull");

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    let backend = match &args.backend {
        Some(name) => cfg.init_named_backend(name).await?,
        None => cfg.init_storage_backend().await?,
    };

    if args.force {
        println!("Resetting local indexes from remote backend...");
        cfg.pull_indexes(&backend).await?;
        println!("Indexes reset from remote");
        return Ok(());
    }

    println!("Pulling and merging indexes from remote backend...");
    let summary = cfg.pull_indexes_merged(&backend, &keys).await?;
    if summary.merged {
        println!("Indexes merged successfully");
    } else {
        println!("No remote indexes found (local indexes unchanged)");
    }
    if !summary.conflicts.is_empty() {
        eprintln!(
            "Warning: {} path conflict(s) after merge (same path, different content):",
            summary.conflicts.len()
        );
        for c in &summary.conflicts {
            eprintln!("  {}", c.path.display());
        }
    }

    Ok(())
}
