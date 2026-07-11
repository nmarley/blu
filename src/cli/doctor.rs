//! Vault health diagnostics (`blu doctor`).

use crate::agent::AgentClient;
use crate::blob::BlobIndex;
use crate::block::{PlainIndex, CURRENT_INDEX_VERSION};
use crate::cli::clapargs::DoctorArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::config::Config;
use crate::dek_provider::DekProvider;
use crate::error::BluError;
use crate::keys::kek::KekStore;

/// Outcome of a single doctor check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Check passed.
    Pass,
    /// Non-fatal issue.
    Warn,
    /// Fatal issue; doctor should exit non-zero.
    Fail,
}

/// One named check in a doctor report.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Short check name.
    pub name: String,
    /// Pass / warn / fail.
    pub status: CheckStatus,
    /// Human-readable detail.
    pub detail: String,
}

/// Full doctor report.
#[derive(Debug, Clone, Default)]
pub struct DoctorReport {
    /// Ordered check results.
    pub checks: Vec<CheckResult>,
}

impl DoctorReport {
    fn push(&mut self, name: impl Into<String>, status: CheckStatus, detail: impl Into<String>) {
        self.checks.push(CheckResult {
            name: name.into(),
            status,
            detail: detail.into(),
        });
    }

    fn pass(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.push(name, CheckStatus::Pass, detail);
    }

    fn warn(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.push(name, CheckStatus::Warn, detail);
    }

    fn fail(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.push(name, CheckStatus::Fail, detail);
    }

    /// True if any check failed.
    pub fn has_failures(&self) -> bool {
        self.checks.iter().any(|c| c.status == CheckStatus::Fail)
    }

    /// Number of failed checks.
    pub fn fail_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Fail)
            .count()
    }

    /// Print the report to stdout.
    pub fn print(&self) {
        for c in &self.checks {
            let label = match c.status {
                CheckStatus::Pass => "ok  ",
                CheckStatus::Warn => "warn",
                CheckStatus::Fail => "FAIL",
            };
            println!("[{}] {}: {}", label, c.name, c.detail);
        }
        let fails = self.fail_count();
        let warns = self
            .checks
            .iter()
            .filter(|c| c.status == CheckStatus::Warn)
            .count();
        if fails == 0 && warns == 0 {
            println!("doctor: all checks passed");
        } else {
            println!("doctor: {} failure(s), {} warning(s)", fails, warns);
        }
    }
}

/// CLI entry point for `blu doctor`.
pub async fn doctor(_args: DoctorArgs) -> Result<(), BluError> {
    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;
    let report = diagnose(&cfg, &keys).await?;
    report.print();
    if report.has_failures() {
        Err(BluError::Internal(format!(
            "doctor found {} issue(s)",
            report.fail_count()
        )))
    } else {
        Ok(())
    }
}

/// Run phase-1 vault health checks without requiring the agent.
///
/// Callers supply an already-loaded config and key provider (agent or local).
pub async fn diagnose(cfg: &Config, keys: &DekProvider) -> Result<DoctorReport, BluError> {
    let mut report = DoctorReport::default();

    check_config(cfg, &mut report);
    check_encryption(cfg, &mut report);
    check_kek_store(cfg, &mut report);
    check_agent(&mut report);

    let plain = match cfg.load_plain_index(keys) {
        Ok(idx) => {
            report.pass(
                "plain-index",
                format!(
                    "decrypted ({} files, {} chunks)",
                    idx.files_map_ref().len(),
                    idx.count_blocks()
                ),
            );
            Some(idx)
        }
        Err(e) => {
            report.fail("plain-index", format!("failed to load: {}", e));
            None
        }
    };

    let blob = match cfg.load_blob_index(keys) {
        Ok(idx) => {
            report.pass(
                "blob-index",
                format!(
                    "decrypted ({} blobs, {} chunks)",
                    idx.count_blob_files(),
                    idx.count_chunks_indexed()
                ),
            );
            Some(idx)
        }
        Err(BluError::IndexNotFound(_)) => {
            report.pass("blob-index", "not present (empty vault)");
            Some(BlobIndex::new())
        }
        Err(e) => {
            report.fail("blob-index", format!("failed to load: {}", e));
            None
        }
    };

    match cfg.load_tag_index(keys) {
        Ok(idx) => {
            report.pass(
                "tag-index",
                format!("decrypted ({} tags)", idx.list_all_tags().len()),
            );
        }
        Err(BluError::IndexNotFound(_)) => {
            report.pass("tag-index", "not present");
        }
        Err(e) => {
            report.fail("tag-index", format!("failed to load: {}", e));
        }
    }

    if let Some(ref plain) = plain {
        check_plain_version(plain, &mut report);
        check_cross_refs(plain, &mut report);
    }

    if let (Some(ref plain), Some(ref blob)) = (&plain, &blob) {
        check_encryption_coverage(plain, blob, &mut report);
        check_gc_queues(blob, &mut report);
        check_blob_presence(cfg, blob, &mut report).await;
        check_blob_orphans(cfg, blob, &mut report).await;
    }

    check_catalog_remote(cfg, keys, &mut report).await;

    Ok(report)
}

/// Report whether the local catalog is fully published to the default backend.
async fn check_catalog_remote(cfg: &Config, keys: &DekProvider, report: &mut DoctorReport) {
    let backend = match cfg.init_storage_backend().await {
        Ok(b) => b,
        Err(e) => {
            report.warn(
                "catalog-remote",
                format!("unable to open default backend: {}", e),
            );
            return;
        }
    };

    match cfg.catalog_remote_state(&backend, keys).await {
        Ok(state) => {
            use crate::config::CatalogRemoteState;
            match state {
                CatalogRemoteState::InSync | CatalogRemoteState::NoRemote => {
                    report.pass("catalog-remote", state.as_str());
                }
                CatalogRemoteState::Ahead => {
                    report.warn(
                        "catalog-remote",
                        "local catalog not fully on remote (run `blu backup`)",
                    );
                }
                CatalogRemoteState::Behind => {
                    report.warn(
                        "catalog-remote",
                        "remote catalog has entries not local (run `blu pull`)",
                    );
                }
                CatalogRemoteState::Diverged => {
                    report.warn(
                        "catalog-remote",
                        "local and remote catalogs diverged (pull, then backup)",
                    );
                }
            }
        }
        Err(e) => {
            report.warn(
                "catalog-remote",
                format!("unable to compare with remote: {}", e),
            );
        }
    }
}

fn check_config(cfg: &Config, report: &mut DoctorReport) {
    if cfg.backends.is_empty() {
        report.fail("config", "no backends configured");
        return;
    }
    if !cfg.backends.contains_key(&cfg.default_backend) {
        report.fail(
            "config",
            format!(
                "default_backend \"{}\" not found in backends",
                cfg.default_backend
            ),
        );
        return;
    }
    report.pass(
        "config",
        format!(
            "{} backend(s), default \"{}\"",
            cfg.backends.len(),
            cfg.default_backend
        ),
    );
}

fn check_encryption(cfg: &Config, report: &mut DoctorReport) {
    if cfg.has_encryption() {
        report.pass("encryption", "pq_recipient configured");
    } else {
        report.fail("encryption", "no encryption configured");
    }
}

fn check_kek_store(cfg: &Config, report: &mut DoctorReport) {
    let store = KekStore::new(&cfg.bludir());
    if !store.exists() {
        report.fail("kek-store", "missing .blu/keys/kek.toml");
        return;
    }
    match store.load_metadata() {
        Ok(meta) => {
            report.pass(
                "kek-store",
                format!(
                    "present (current version {}, {} recorded version(s))",
                    meta.current_version,
                    meta.versions.len()
                ),
            );
        }
        Err(e) => {
            report.fail("kek-store", format!("metadata unreadable: {}", e));
        }
    }
}

fn check_agent(report: &mut DoctorReport) {
    let client = match AgentClient::new() {
        Ok(c) => c,
        Err(e) => {
            report.warn("agent", format!("unavailable: {}", e));
            return;
        }
    };
    if !client.is_running() {
        report.warn("agent", "daemon not running");
        return;
    }
    match client.status() {
        Ok(resp) => {
            let unlocked = resp["result"]["unlocked"].as_bool().unwrap_or(false);
            if unlocked {
                report.pass("agent", "running, unlocked");
            } else {
                report.warn("agent", "running, locked");
            }
        }
        Err(e) => {
            report.warn("agent", format!("status failed: {}", e));
        }
    }
}

fn check_plain_version(plain: &PlainIndex, report: &mut DoctorReport) {
    if plain.version == CURRENT_INDEX_VERSION {
        report.pass(
            "index-version",
            format!("plain index version {}", plain.version),
        );
    } else {
        report.warn(
            "index-version",
            format!(
                "plain index version {} (current is {})",
                plain.version, CURRENT_INDEX_VERSION
            ),
        );
    }
}

fn check_cross_refs(plain: &PlainIndex, report: &mut DoctorReport) {
    let mut issues = 0usize;

    for (file_hash, fileref) in plain.files_map_ref() {
        if fileref.paths.is_empty() {
            issues += 1;
        }
        for cm in &fileref.chunkmetas {
            match plain.blocks_map_ref().get(&cm.hash) {
                None => issues += 1,
                Some(blockref) => {
                    if !blockref.references.contains_key(file_hash) {
                        issues += 1;
                    }
                }
            }
        }
    }

    for (block_hash, blockref) in plain.blocks_map_ref() {
        if blockref.references.is_empty() {
            issues += 1;
            continue;
        }
        for file_hash in blockref.references.keys() {
            match plain.files_map_ref().get(file_hash) {
                None => issues += 1,
                Some(fileref) => {
                    if !fileref.chunkmetas.iter().any(|cm| &cm.hash == block_hash) {
                        issues += 1;
                    }
                }
            }
        }
    }

    if issues == 0 {
        report.pass("cross-refs", "plain index internal refs consistent");
    } else {
        report.fail(
            "cross-refs",
            format!("{} inconsistency(ies) in plain index refs", issues),
        );
    }
}

fn check_encryption_coverage(plain: &PlainIndex, blob: &BlobIndex, report: &mut DoctorReport) {
    let total = plain.count_blocks();
    if total == 0 {
        report.pass("encryption-coverage", "no chunks in plain index");
        return;
    }
    let encrypted = plain
        .blocks_map_ref()
        .keys()
        .filter(|h| blob.has_chunk(h))
        .count();
    let pct = (encrypted as f64 / total as f64) * 100.0;
    if encrypted == total {
        report.pass(
            "encryption-coverage",
            format!("{}/{} chunks encrypted ({:.1}%)", encrypted, total, pct),
        );
    } else {
        report.warn(
            "encryption-coverage",
            format!(
                "{}/{} chunks encrypted ({:.1}%); {} unencrypted",
                encrypted,
                total,
                pct,
                total - encrypted
            ),
        );
    }
}

fn check_gc_queues(blob: &BlobIndex, report: &mut DoctorReport) {
    let delete_n = blob.paths_to_delete.len();
    let repack_n = blob.paths_to_repack.len();
    if delete_n == 0 && repack_n == 0 {
        report.pass("gc-queues", "no pending delete or repack");
    } else {
        report.warn(
            "gc-queues",
            format!(
                "{} path(s) pending delete, {} path(s) pending repack",
                delete_n, repack_n
            ),
        );
    }
}

async fn check_blob_presence(cfg: &Config, blob: &BlobIndex, report: &mut DoctorReport) {
    if blob.path_index.is_empty() {
        report.pass("blob-presence", "no indexed blobs");
        return;
    }

    let backend = match cfg.init_storage_backend().await {
        Ok(b) => b,
        Err(e) => {
            report.fail("blob-presence", format!("backend init failed: {}", e));
            return;
        }
    };

    let mut missing = 0usize;
    let mut checked = 0usize;
    for path in blob.path_index.keys() {
        checked += 1;
        match backend.exists(path).await {
            Ok(true) => {}
            Ok(false) => missing += 1,
            Err(e) => {
                report.fail(
                    "blob-presence",
                    format!("exists check failed for {}: {}", path.display(), e),
                );
                return;
            }
        }
    }

    if missing == 0 {
        report.pass(
            "blob-presence",
            format!("all {} indexed blob path(s) present on backend", checked),
        );
    } else {
        report.fail(
            "blob-presence",
            format!(
                "{}/{} indexed blob path(s) missing from backend",
                missing, checked
            ),
        );
    }
}

/// Cap orphan path samples in doctor output.
const MAX_ORPHAN_SAMPLES: usize = 5;

/// Backend objects not referenced by the blob index (orphans).
///
/// Warn-only: publish completeness should prevent this; multi-device-safe
/// reclaim is a separate follow-up.
async fn check_blob_orphans(cfg: &Config, blob: &BlobIndex, report: &mut DoctorReport) {
    let backend = match cfg.init_storage_backend().await {
        Ok(b) => b,
        Err(e) => {
            report.warn("blob-orphans", format!("backend init failed: {}", e));
            return;
        }
    };

    let listed = match backend.list_blob_paths().await {
        Ok(paths) => paths,
        Err(e) => {
            report.warn(
                "blob-orphans",
                format!("unable to list backend blobs: {}", e),
            );
            return;
        }
    };

    let indexed: std::collections::HashSet<_> = blob.path_index.keys().cloned().collect();
    let mut orphans: Vec<_> = listed
        .into_iter()
        .filter(|p| !indexed.contains(p))
        .collect();
    orphans.sort();

    if orphans.is_empty() {
        report.pass(
            "blob-orphans",
            format!(
                "no unreferenced blob objects ({} indexed path(s))",
                indexed.len()
            ),
        );
        return;
    }

    let sample: Vec<String> = orphans
        .iter()
        .take(MAX_ORPHAN_SAMPLES)
        .map(|p| p.display().to_string())
        .collect();
    let more = orphans.len().saturating_sub(sample.len());
    let mut detail = format!("{} backend blob object(s) not in catalog", orphans.len());
    if !sample.is_empty() {
        detail.push_str(": ");
        detail.push_str(&sample.join(", "));
        if more > 0 {
            detail.push_str(&format!(" … and {} more", more));
        }
    }
    report.warn("blob-orphans", detail);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::BlobBlockLocation;
    use crate::cli::{init_vault, InitVaultParams};
    use crate::hash::Hash;
    use crate::io::Position;
    use crate::keys::mnemonic;
    use age::Identity;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn test_pq_recipient() -> String {
        let m = mnemonic::parse_mnemonic(mnemonic::TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");
        mnemonic::derive_pq_recipient(&seed).unwrap().to_string()
    }

    fn local_keys(cfg: &Config) -> DekProvider {
        let store = KekStore::new(&cfg.bludir());
        let m = mnemonic::parse_mnemonic(mnemonic::TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");
        let pq_identity = mnemonic::derive_pq_identity(&seed).unwrap();
        let (kek, version) = store
            .unwrap_current_kek_with(&[&pq_identity as &dyn Identity])
            .unwrap();
        DekProvider::Local {
            kek,
            kek_version: version,
        }
    }

    fn setup_vault() -> (tempfile::TempDir, Config, DekProvider) {
        let tmp = tempdir().unwrap();
        init_vault(
            tmp.path(),
            InitVaultParams {
                pq_recipient: test_pq_recipient(),
            },
        )
        .unwrap();
        // Ensure local backend path is absolute so tests work without chdir.
        let mut cfg = crate::config::read_config(tmp.path()).unwrap();
        let data_path = tmp.path().join(".blu/data");
        fs::create_dir_all(&data_path).unwrap();
        let default_name = cfg.default_backend.clone();
        if let Some(crate::config::backend::BackendConfig::Local(ref mut local)) =
            cfg.backends.get_mut(&default_name)
        {
            local.path = data_path;
        }
        cfg.save().unwrap();
        let cfg = crate::config::read_config(tmp.path()).unwrap();
        let keys = local_keys(&cfg);
        (tmp, cfg, keys)
    }

    #[tokio::test]
    async fn healthy_empty_vault_passes() {
        let (_tmp, cfg, keys) = setup_vault();
        let report = diagnose(&cfg, &keys).await.unwrap();
        assert!(
            !report.has_failures(),
            "unexpected failures: {:?}",
            report.checks
        );
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "plain-index" && c.status == CheckStatus::Pass));
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "blob-presence" && c.status == CheckStatus::Pass));
    }

    #[tokio::test]
    async fn missing_blob_path_fails() {
        let (_tmp, cfg, keys) = setup_vault();

        let mut blob = BlobIndex::new();
        let fake = Path::new("a/aaa/aaaaaaaa_missing_blob");
        let location = BlobBlockLocation::new(
            fake.to_path_buf(),
            Position {
                offset: 0,
                size: 16,
            },
        );
        let chunk = Hash::from(crate::hash::multihash(b"doctor-missing-blob-test").to_bytes());
        blob.add_chunk_location(&chunk, &location);
        cfg.write_blob_index(&blob, &keys).unwrap();

        let report = diagnose(&cfg, &keys).await.unwrap();
        assert!(report.has_failures());
        let presence = report
            .checks
            .iter()
            .find(|c| c.name == "blob-presence")
            .expect("blob-presence check");
        assert_eq!(presence.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn orphan_blob_on_backend_warns() {
        let (_tmp, cfg, keys) = setup_vault();

        let backend = cfg.init_storage_backend().await.unwrap();
        let data = b"orphan-blob-bytes";
        let hash = Hash::from(crate::hash::multihash(data).to_bytes());
        let orphan_path = backend.write_data(&hash, data).await.unwrap();

        // Empty blob index: the written object is unreferenced.
        cfg.write_blob_index(&BlobIndex::new(), &keys).unwrap();

        let report = diagnose(&cfg, &keys).await.unwrap();
        assert!(
            !report.has_failures(),
            "orphans are warn-only: {:?}",
            report.checks
        );
        let orphans = report
            .checks
            .iter()
            .find(|c| c.name == "blob-orphans")
            .expect("blob-orphans check");
        assert_eq!(orphans.status, CheckStatus::Warn);
        assert!(
            orphans.detail.contains("1 backend blob object"),
            "detail={}",
            orphans.detail
        );
        assert!(
            orphans.detail.contains(&orphan_path.display().to_string())
                || orphans.detail.contains("not in catalog"),
            "detail={}",
            orphans.detail
        );
    }

    #[tokio::test]
    async fn corrupt_cross_ref_fails() {
        let (tmp, cfg, keys) = setup_vault();

        let file_path = tmp.path().join("only.txt");
        fs::write(&file_path, b"hello doctor").unwrap();
        let mut plain = PlainIndex::new_empty();
        plain.add(&file_path, None).unwrap();

        // Drop all blocks to force a dangling chunkmeta reference.
        plain.blocks.clear();
        cfg.write_plain_index(&plain, &keys).unwrap();

        let report = diagnose(&cfg, &keys).await.unwrap();
        assert!(report.has_failures());
        let xref = report
            .checks
            .iter()
            .find(|c| c.name == "cross-refs")
            .expect("cross-refs check");
        assert_eq!(xref.status, CheckStatus::Fail);
    }
}
