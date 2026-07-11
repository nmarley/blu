//! Vault status: working tree vs catalog vs remote.
//!
//! Answers three questions:
//! 1. What local files are not yet in the catalog?
//! 2. What catalog entries are not checked out on disk?
//! 3. Is the local catalog in sync with, ahead of, behind, or diverged
//!    from the remote?

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::block::PlainIndex;
use crate::cli::clapargs::{StatusArgs, StatusCheckType};
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::cli::output::FileDisplay;
use crate::config::backend::BackendConfig;
use crate::config::{CatalogRemoteState, Config};
use crate::error::BluError;
use crate::format::human_bytes;
use crate::hash::{self, Hash};
use crate::ignore::walk_files_with_sizes;

/// Cap long file lists so status stays readable on large vaults.
const MAX_LISTED_PATHS: usize = 20;

/// Default to shallow path/size checks above this working-tree size.
const SHALLOW_CHECK_BYTE_COUNT: u64 = 1024 * 1024 * 1024;

/// Show working tree vs catalog vs remote.
pub async fn status(args: StatusArgs) -> Result<(), BluError> {
    let dir = Path::new(".");
    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;
    let index = cfg.load_plain_index_or_default(&keys);

    let files_list = walk_files_with_sizes(dir);
    let current_dir_size = files_list.iter().fold(0u64, |acc, elem| acc + elem.1);
    let status_check_type = match args.status_check_type {
        Some(t) => t,
        None => {
            if current_dir_size >= SHALLOW_CHECK_BYTE_COUNT {
                StatusCheckType::Shallow
            } else {
                StatusCheckType::Deep
            }
        }
    };

    let mut unpublished: Vec<FileDisplay> = Vec::new();
    let mut path_updates: HashSet<FileUpdatedPaths> = HashSet::new();
    let mut size_mismatches: Vec<FileSizeMismatch> = Vec::new();

    match status_check_type {
        StatusCheckType::Shallow => {
            collect_shallow_unpublished(
                &index,
                &files_list,
                &mut unpublished,
                &mut size_mismatches,
            );
        }
        StatusCheckType::Deep => {
            collect_deep_unpublished(&index, &mut unpublished, &mut path_updates)?;
        }
    }

    let checkout = checkout_summary(&index);
    let remote_state = remote_catalog_state(&cfg, &keys).await;

    print_header(&cfg);
    print_catalog_summary(&index, remote_state);
    print_checkout_summary(&checkout);
    print_section("Unpublished local files", &unpublished, |f| f.to_string());
    print_missing_checkout(&checkout);
    print_extra_changes(&path_updates, &size_mismatches);

    Ok(())
}

fn print_header(cfg: &Config) {
    let vault = cfg.basedir().display();
    let backend = format_default_backend(cfg);
    println!("On vault {}  backend {}", vault, backend);
}

fn format_default_backend(cfg: &Config) -> String {
    let name = &cfg.default_backend;
    match cfg.backends.get(name) {
        Some(BackendConfig::Local(local)) => {
            format!("{} (local:{})", name, local.path.display())
        }
        Some(BackendConfig::AmazonS3(s3)) => {
            let prefix = s3.prefix.as_deref().unwrap_or("");
            if prefix.is_empty() {
                format!("{} (s3://{})", name, s3.bucket)
            } else {
                format!("{} (s3://{}/{})", name, s3.bucket, prefix)
            }
        }
        None => format!("{} (missing)", name),
    }
}

fn print_catalog_summary(index: &PlainIndex, remote: RemoteStateLine) {
    let file_count = index.files_map_ref().len();
    let total_bytes = index.total_bytes_indexed();
    println!(
        "Catalog: {} files ({})    Remote: {}",
        file_count,
        human_bytes(total_bytes),
        remote.display(),
    );
}

fn print_checkout_summary(checkout: &CheckoutSummary) {
    if checkout.catalog_entries == 0 {
        println!("Checkout: empty catalog");
        return;
    }
    let mut line = format!(
        "Checkout: {} present, {} missing ({})",
        checkout.present,
        checkout.missing_count(),
        human_bytes(checkout.missing_bytes),
    );
    if checkout.missing_count() > 0 {
        line.push_str(" — blu restore --path '…'  or  blu restore --all");
    }
    println!("{}", line);
}

fn print_section<T, F>(title: &str, items: &[T], fmt: F)
where
    F: Fn(&T) -> String,
{
    if items.is_empty() {
        return;
    }
    println!();
    println!("{}:", title);
    let shown = items.len().min(MAX_LISTED_PATHS);
    for item in items.iter().take(shown) {
        println!("  {}", fmt(item));
    }
    if items.len() > shown {
        println!("  … and {} more", items.len() - shown);
    }
}

fn print_missing_checkout(checkout: &CheckoutSummary) {
    if checkout.missing.is_empty() {
        return;
    }
    println!();
    println!("Not in checkout (in catalog only):");
    let shown = checkout.missing.len().min(MAX_LISTED_PATHS);
    for entry in checkout.missing.iter().take(shown) {
        println!(
            "  {}  {}  {}",
            entry.hash.dbg_short(7),
            human_bytes(entry.size),
            entry.path.display()
        );
    }
    if checkout.missing.len() > shown {
        println!("  … and {} more", checkout.missing.len() - shown);
    }
}

fn print_extra_changes(
    path_updates: &HashSet<FileUpdatedPaths>,
    size_mismatches: &[FileSizeMismatch],
) {
    if path_updates.is_empty() && size_mismatches.is_empty() {
        return;
    }
    println!();
    println!("Other changes:");
    for fref in path_updates {
        println!("  renamed:  {}", fref);
    }
    for fref in size_mismatches {
        println!("  modified: {}", fref);
    }
}

enum RemoteStateLine {
    Ok(CatalogRemoteState),
    Error(String),
}

impl RemoteStateLine {
    fn display(&self) -> String {
        match self {
            Self::Ok(state) => state.as_str().to_string(),
            Self::Error(msg) => format!("unavailable ({})", msg),
        }
    }
}

async fn remote_catalog_state(
    cfg: &Config,
    keys: &crate::dek_provider::DekProvider,
) -> RemoteStateLine {
    match cfg.init_storage_backend().await {
        Ok(backend) => match cfg.catalog_remote_state(&backend, keys).await {
            Ok(state) => RemoteStateLine::Ok(state),
            Err(e) => RemoteStateLine::Error(e.to_string()),
        },
        Err(e) => RemoteStateLine::Error(e.to_string()),
    }
}

/// One catalog path that is not present on disk.
#[derive(Debug, Clone)]
struct MissingCheckout {
    hash: Hash,
    size: u64,
    path: PathBuf,
}

/// Checkout presence for catalog entries.
#[derive(Debug, Default)]
struct CheckoutSummary {
    catalog_entries: usize,
    present: usize,
    missing_bytes: u64,
    missing: Vec<MissingCheckout>,
}

impl CheckoutSummary {
    fn missing_count(&self) -> usize {
        self.missing.len()
    }
}

/// Count catalog paths present on disk vs missing (not checked out).
fn checkout_summary(index: &PlainIndex) -> CheckoutSummary {
    let mut summary = CheckoutSummary {
        catalog_entries: index.files_map_ref().len(),
        ..CheckoutSummary::default()
    };

    for (hash, fileref) in index.files_map_ref() {
        let size = fileref.total_size();
        // A file is present if any of its recorded paths exists on disk.
        let any_present = fileref.paths.iter().any(|p| p.exists());
        if any_present {
            summary.present += 1;
            continue;
        }
        summary.missing_bytes += size;
        // Prefer the first path for display.
        if let Some(path) = fileref.paths.iter().next() {
            summary.missing.push(MissingCheckout {
                hash: hash.clone(),
                size,
                path: path.clone(),
            });
        } else {
            summary.missing.push(MissingCheckout {
                hash: hash.clone(),
                size,
                path: PathBuf::from("(no path)"),
            });
        }
    }

    summary.missing.sort_by(|a, b| a.path.cmp(&b.path));
    summary
}

fn collect_shallow_unpublished(
    index: &PlainIndex,
    files_list: &[(PathBuf, u64)],
    unpublished: &mut Vec<FileDisplay>,
    size_mismatches: &mut Vec<FileSizeMismatch>,
) {
    let path_index = index.build_path_index();
    for (p, s) in files_list {
        match path_index.get(p) {
            None => {
                let bytes = match std::fs::read(p) {
                    Ok(b) => b,
                    Err(e) => {
                        println!("unable to read file {:?}: {}", p, e);
                        continue;
                    }
                };
                let hash = Hash::from(hash::multihash(&bytes).to_bytes());
                unpublished.push(FileDisplay {
                    hash,
                    size: *s,
                    paths: vec![p.clone()],
                });
            }
            Some(val) => {
                let Some(fileref) = index.get_fileref_ref(val) else {
                    println!("error: unable to get fileref for {:?}", p);
                    continue;
                };
                let size_in_index = fileref.total_size();
                if size_in_index != *s {
                    size_mismatches.push(FileSizeMismatch {
                        hash: val.clone(),
                        size_in_index,
                        size_in_filesystem: *s,
                        paths: fileref.paths.iter().cloned().collect(),
                    });
                }
            }
        }
    }
}

fn collect_deep_unpublished(
    index: &PlainIndex,
    unpublished: &mut Vec<FileDisplay>,
    path_updates: &mut HashSet<FileUpdatedPaths>,
) -> Result<(), BluError> {
    let curr_fs_index = PlainIndex::new(".")?;

    for (file_hash, fileref) in &curr_fs_index.files {
        match index.files.get(file_hash) {
            Some(val) => {
                if val.paths != fileref.paths {
                    path_updates.insert(FileUpdatedPaths {
                        hash: file_hash.clone(),
                        size: fileref.total_size(),
                        paths_in_index: val.paths.iter().cloned().collect(),
                        paths_in_filesystem: fileref.paths.iter().cloned().collect(),
                    });
                }
            }
            None => {
                unpublished.push(FileDisplay {
                    hash: file_hash.clone(),
                    size: fileref.total_size(),
                    paths: fileref.paths.iter().cloned().collect(),
                });
            }
        }
    }

    // Paths recorded in the catalog that differ on disk (rename detection).
    for (file_hash, fileref) in &index.files {
        if let Some(val) = curr_fs_index.files.get(file_hash) {
            if val.paths != fileref.paths {
                path_updates.insert(FileUpdatedPaths {
                    hash: file_hash.clone(),
                    size: fileref.total_size(),
                    paths_in_index: fileref.paths.iter().cloned().collect(),
                    paths_in_filesystem: val.paths.iter().cloned().collect(),
                });
            }
        }
    }

    Ok(())
}

#[derive(Clone, Debug)]
struct FileSizeMismatch {
    hash: Hash,
    size_in_index: u64,
    size_in_filesystem: u64,
    paths: Vec<PathBuf>,
}

impl std::fmt::Display for FileSizeMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "hash: {}, index_size: {}, fs_size: {}, paths: {:?}",
            self.hash.dbg_short(7),
            self.size_in_index,
            self.size_in_filesystem,
            self.paths,
        )
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct FileUpdatedPaths {
    hash: Hash,
    size: u64,
    paths_in_index: Vec<PathBuf>,
    paths_in_filesystem: Vec<PathBuf>,
}

impl std::fmt::Display for FileUpdatedPaths {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "hash: {}, size: {}, index_paths: {:?}, fs_paths: {:?}",
            self.hash.dbg_short(7),
            self.size,
            self.paths_in_index,
            self.paths_in_filesystem,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::block::PlainIndex;

    #[test]
    fn checkout_summary_counts_missing_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let present = tmp.path().join("present.txt");
        fs::write(&present, b"hello").unwrap();

        let mut index = PlainIndex::new_empty();
        index.add(&present, None).unwrap();

        // Add a catalog-only path by cloning a fileref and swapping paths.
        // Easier: index a file then delete it from disk.
        let gone = tmp.path().join("gone.txt");
        fs::write(&gone, b"bye").unwrap();
        index.add(&gone, None).unwrap();
        fs::remove_file(&gone).unwrap();

        let summary = checkout_summary(&index);
        assert_eq!(summary.catalog_entries, 2);
        assert_eq!(summary.present, 1);
        assert_eq!(summary.missing_count(), 1);
        assert_eq!(summary.missing_bytes, 3); // "bye"
        assert_eq!(summary.missing[0].path, gone);
    }
}
