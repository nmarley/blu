//! Pull command: refresh the local catalog from the remote backend.
//!
//! Pull never materializes plaintext. After a successful pull, missing
//! checkout paths are reported with a `blu restore` hint.

use crate::block::PlainIndex;
use crate::cli::clapargs::PullArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::config::Config;
use crate::dek_provider::DekProvider;
use crate::error::BluError;
use crate::format::human_bytes;

/// Fetch and merge remote catalog indexes (no plaintext).
///
/// Default: union-merge remote into local (keeps local-only entries).
/// `--force` discards local indexes and takes the remote copy only.
pub async fn pull(args: PullArgs) -> Result<(), BluError> {
    info!("Started pull");

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    let backend = match &args.backend {
        Some(name) => cfg.init_named_backend(name).await?,
        None => cfg.init_storage_backend().await?,
    };

    if args.force {
        println!("Resetting local catalog from remote (hard reset)...");
        cfg.pull_indexes(&backend).await?;
        println!("Catalog reset from remote (indexes only; no plaintext written).");
        print_restore_hint(&cfg, &keys);
        return Ok(());
    }

    println!("Pulling catalog from remote (indexes only; no plaintext)...");
    let summary = cfg.pull_indexes_merged(&backend, &keys).await?;
    if summary.merged {
        println!("Catalog merged successfully.");
    } else {
        println!("No remote catalog found (local indexes unchanged).");
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

    print_restore_hint(&cfg, &keys);
    Ok(())
}

/// If catalog entries are missing on disk, remind the user to restore.
fn print_restore_hint(cfg: &Config, keys: &DekProvider) {
    let index = cfg.load_plain_index_or_default(keys);
    let (missing, missing_bytes) = count_missing_checkout(&index);
    if missing == 0 {
        return;
    }
    println!(
        "Checkout: {} catalog file(s) not on disk ({}) — run `blu restore --path '…'` or `blu restore --all`.",
        missing,
        human_bytes(missing_bytes),
    );
}

/// Count catalog files with no path present on disk, and their total size.
fn count_missing_checkout(index: &PlainIndex) -> (usize, u64) {
    let mut missing = 0usize;
    let mut missing_bytes = 0u64;
    for fileref in index.files_map_ref().values() {
        let any_present = fileref.paths.iter().any(|p| p.exists());
        if !any_present {
            missing += 1;
            missing_bytes += fileref.total_size();
        }
    }
    (missing, missing_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn count_missing_checkout_detects_absent_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let present = tmp.path().join("present.txt");
        fs::write(&present, b"hello").unwrap();

        let mut index = PlainIndex::new_empty();
        index.add(&present, None).unwrap();

        let gone = tmp.path().join("gone.txt");
        fs::write(&gone, b"bye").unwrap();
        index.add(&gone, None).unwrap();
        fs::remove_file(&gone).unwrap();

        let (missing, missing_bytes) = count_missing_checkout(&index);
        assert_eq!(missing, 1);
        assert_eq!(missing_bytes, 3);
    }
}
