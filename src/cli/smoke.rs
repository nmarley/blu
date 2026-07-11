//! End-to-end smoke tests for the main vault data pipeline.
//!
//! Exercises library APIs that back the CLI (init → sync → list/status →
//! restore → delete → doctor) without requiring the agent daemon.
//!
//! Multi-device smokes (shared local backend, two vault dirs) lock the
//! git-like multi-writer acceptance criteria.

#![cfg(test)]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use age::Identity;
use tempfile::tempdir;

use crate::blob::{BlobBuffer, BlobIndex, EncBlobReader};
use crate::block::PlainIndex;
use crate::cli::doctor::{diagnose, CheckStatus};
use crate::cli::{init_vault, open_vault, InitVaultParams, OpenVaultParams};
use crate::config::{self, backend::BackendConfig, Config};
use crate::dek_provider::DekProvider;
use crate::hash::Hash;
use crate::keys::kek::KekStore;
use crate::keys::mnemonic;
use crate::storage::BackendKind;

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

    let mut cfg = config::read_config(tmp.path()).unwrap();
    let data_path = tmp.path().join(".blu/data");
    fs::create_dir_all(&data_path).unwrap();
    let default_name = cfg.default_backend.clone();
    if let Some(config::backend::BackendConfig::Local(ref mut local)) =
        cfg.backends.get_mut(&default_name)
    {
        local.path = data_path;
    }
    cfg.save().unwrap();
    let cfg = config::read_config(tmp.path()).unwrap();
    let keys = local_keys(&cfg);
    (tmp, cfg, keys)
}

async fn sync_tree(cfg: &Config, keys: &DekProvider, paths: &[PathBuf]) -> (PlainIndex, BlobIndex) {
    let mut plain = cfg.load_plain_index_or_default(keys);
    for p in paths {
        plain.add(p, None).unwrap();
    }
    cfg.write_plain_index(&plain, keys).unwrap();

    let mut blob = cfg.load_blob_index_or_default(keys);
    let backend = cfg.init_storage_backend().await.unwrap();
    let mut blob_buf = BlobBuffer::new(&backend, keys.clone());

    let file_hashes: Vec<Hash> = plain.files_map_ref().keys().cloned().collect();
    let mut chunks_encrypted = 0usize;
    for file_hash in &file_hashes {
        let file_ref = plain.get_fileref_ref(file_hash).unwrap();
        for cm in &file_ref.chunkmetas {
            if blob.has_chunk(&cm.hash) {
                continue;
            }
            let block_ref = plain.blocks_map_ref().get(&cm.hash).unwrap();
            let mut data = plain.read_block_bytes(block_ref).unwrap();
            blob_buf.add_chunk(&mut data, &mut blob).await.unwrap();
            chunks_encrypted += 1;
        }
    }
    if chunks_encrypted > 0 {
        blob_buf.finalize(&mut blob).await.unwrap();
        cfg.write_blob_index(&blob, keys).unwrap();
    }
    (plain, blob)
}

/// Sync local paths then merge-remote + push indexes (CLI `blu sync` shape).
async fn sync_tree_and_push(
    cfg: &Config,
    keys: &DekProvider,
    paths: &[PathBuf],
) -> (PlainIndex, BlobIndex) {
    let (_plain, _blob) = sync_tree(cfg, keys, paths).await;
    let backend = cfg.init_storage_backend().await.unwrap();
    cfg.merge_remote_indexes(&backend, keys).await.unwrap();
    // Re-load after merge so callers see the merged plain index.
    let plain = cfg.load_plain_index_or_default(keys);
    let blob = cfg.load_blob_index_or_default(keys);
    cfg.push_indexes(&backend).await.unwrap();
    (plain, blob)
}

fn point_at_shared_backend(vault_dir: &Path, backend_dir: &Path) -> Config {
    let mut cfg = config::read_config(vault_dir).unwrap();
    fs::create_dir_all(backend_dir).unwrap();
    cfg.backends.clear();
    cfg.backends.insert(
        "remote".into(),
        BackendConfig::Local(config::backend::LocalConfig {
            path: backend_dir.to_path_buf(),
        }),
    );
    cfg.default_backend = "remote".into();
    cfg.save().unwrap();
    config::read_config(vault_dir).unwrap()
}

/// Vault A owns the KEK; shared backend holds blobs + indexes.
async fn setup_primary_vault(vault_dir: &Path, backend_dir: &Path) -> (Config, DekProvider) {
    fs::create_dir_all(vault_dir).unwrap();
    init_vault(
        vault_dir,
        InitVaultParams {
            pq_recipient: test_pq_recipient(),
        },
    )
    .unwrap();
    let cfg = point_at_shared_backend(vault_dir, backend_dir);
    let keys = local_keys(&cfg);
    let backend = cfg.init_storage_backend().await.unwrap();
    cfg.push_indexes(&backend).await.unwrap();
    (cfg, keys)
}

/// Second machine: open existing vault from the shared backend.
async fn setup_secondary_vault(vault_dir: &Path, backend_dir: &Path) -> (Config, DekProvider) {
    open_vault(OpenVaultParams {
        dir: vault_dir.to_path_buf(),
        pq_recipient: test_pq_recipient(),
        backend_name: "remote".into(),
        backend: BackendConfig::Local(config::backend::LocalConfig {
            path: backend_dir.to_path_buf(),
        }),
    })
    .await
    .unwrap();
    let cfg = config::read_config(vault_dir).unwrap();
    let keys = local_keys(&cfg);
    (cfg, keys)
}

async fn pull_indexes(cfg: &Config, keys: &DekProvider) {
    let backend = cfg.init_storage_backend().await.unwrap();
    cfg.pull_indexes_merged(&backend, keys).await.unwrap();
}

fn basenames(plain: &PlainIndex) -> HashSet<String> {
    list_paths(plain)
        .into_iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

async fn restore_file_bytes(
    plain: &PlainIndex,
    blob: &BlobIndex,
    keys: &DekProvider,
    backend: &BackendKind,
    file_hash: &Hash,
) -> Vec<u8> {
    let fileref = plain.get_fileref_ref(file_hash).unwrap();
    let reader = EncBlobReader::new(keys.clone(), backend.clone());
    let mut out = Vec::with_capacity(fileref.total_size() as usize);
    for cm in &fileref.chunkmetas {
        let location = blob.get_block_location_ref(&cm.hash).unwrap();
        let chunk = reader.get_bytes(&location).await.unwrap();
        assert_eq!(chunk.len(), cm.size);
        out.extend_from_slice(&chunk);
    }
    out
}

async fn delete_all_files(cfg: &Config, keys: &DekProvider) -> (PlainIndex, BlobIndex) {
    let mut plain = cfg.load_plain_index(keys).unwrap();
    let mut blob = cfg.load_blob_index_or_default(keys);
    let mut tags = cfg.load_tag_index_or_default(keys);

    let hashes: Vec<Hash> = plain.files_map_ref().keys().cloned().collect();
    for file_hash in &hashes {
        let chunk_hashes: Vec<Hash> = plain
            .get_fileref_ref(file_hash)
            .map(|fr| fr.chunkmetas.iter().map(|cm| cm.hash.clone()).collect())
            .unwrap_or_default();

        plain.tombstone_file(file_hash);
        for chunk_hash in &chunk_hashes {
            if plain.blocks_map_ref().contains_key(chunk_hash) {
                continue;
            }
            if blob.has_chunk(chunk_hash) {
                blob.delete_chunk(chunk_hash).unwrap();
            }
        }
        tags.drop_all_tags(file_hash);
    }

    let dead = blob.drain_paths_to_delete();
    if !dead.is_empty() {
        let backend = cfg.init_storage_backend().await.unwrap();
        for path in &dead {
            backend.delete(path).await.unwrap();
        }
    }

    cfg.write_plain_index(&plain, keys).unwrap();
    cfg.write_blob_index(&blob, keys).unwrap();
    cfg.write_tag_index(&tags, keys).unwrap();
    (plain, blob)
}

fn list_paths(plain: &PlainIndex) -> HashSet<PathBuf> {
    plain
        .files_map_ref()
        .values()
        .flat_map(|fr| fr.paths.iter().cloned())
        .collect()
}

#[tokio::test]
async fn vault_pipeline_happy_path() {
    let (tmp, cfg, keys) = setup_vault();

    // Small tree to sync
    let docs = tmp.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    let a = docs.join("a.txt");
    let b = docs.join("b.txt");
    fs::write(&a, b"alpha content for smoke").unwrap();
    fs::write(&b, b"bravo content for smoke test").unwrap();

    // sync
    let (plain, blob) = sync_tree(&cfg, &keys, std::slice::from_ref(&docs)).await;
    assert_eq!(plain.files_map_ref().len(), 2);
    assert!(blob.count_blob_files() >= 1);
    assert_eq!(
        plain.count_blocks(),
        plain
            .blocks_map_ref()
            .keys()
            .filter(|h| blob.has_chunk(h))
            .count(),
        "all chunks should be encrypted after sync"
    );

    // list
    let paths = list_paths(&plain);
    assert!(paths.iter().any(|p| p.ends_with("a.txt")));
    assert!(paths.iter().any(|p| p.ends_with("b.txt")));

    // status-like summary: encryption coverage + doctor
    let report = diagnose(&cfg, &keys).await.unwrap();
    assert!(
        !report.has_failures(),
        "doctor should be clean after sync: {:?}",
        report.checks
    );
    assert!(report
        .checks
        .iter()
        .any(|c| { c.name == "encryption-coverage" && c.status == CheckStatus::Pass }));

    // restore both files and compare bytes
    let backend = cfg.init_storage_backend().await.unwrap();
    for (file_hash, fileref) in plain.files_map_ref() {
        let restored = restore_file_bytes(&plain, &blob, &keys, &backend, file_hash).await;
        let src_path = fileref.paths.iter().next().unwrap();
        let original = fs::read(src_path).unwrap();
        assert_eq!(restored, original, "restore mismatch for {:?}", src_path);
    }

    // delete all → empty indexes, doctor still clean (empty vault)
    let (plain_after, blob_after) = delete_all_files(&cfg, &keys).await;
    assert!(plain_after.files_map_ref().is_empty());
    assert!(plain_after.blocks_map_ref().is_empty());
    assert_eq!(blob_after.count_chunks_indexed(), 0);

    let report_after = diagnose(&cfg, &keys).await.unwrap();
    assert!(
        !report_after.has_failures(),
        "doctor should be clean after delete: {:?}",
        report_after.checks
    );
}

#[tokio::test]
async fn doctor_fails_on_missing_blob_after_sync() {
    let (tmp, cfg, keys) = setup_vault();

    let f = tmp.path().join("solo.txt");
    fs::write(&f, b"will lose its blob").unwrap();
    let (_plain, blob) = sync_tree(&cfg, &keys, &[f]).await;
    assert!(blob.count_blob_files() >= 1);

    // Delete every blob object from the backend while leaving the index.
    let backend = cfg.init_storage_backend().await.unwrap();
    for path in blob.path_index.keys() {
        backend.delete(path).await.unwrap();
    }

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
async fn bluignore_respected_during_sync_walk() {
    let (tmp, cfg, keys) = setup_vault();

    fs::write(tmp.path().join(".bluignore"), "*.log\n").unwrap();
    fs::write(tmp.path().join("keep.txt"), b"keep me").unwrap();
    fs::write(tmp.path().join("noise.log"), b"ignore me").unwrap();

    let (plain, _blob) = sync_tree(&cfg, &keys, &[tmp.path().to_path_buf()]).await;
    let paths = list_paths(&plain);
    assert!(paths.iter().any(|p| p.ends_with("keep.txt")));
    assert!(
        !paths.iter().any(|p| p.ends_with("noise.log")),
        "ignored file should not be indexed: {:?}",
        paths
    );
}

/// Sequential multi-writer: each side pulls before the next publish.
///
/// With LWW full-index replace this already works when machines always
/// pull before mutating. Locks acceptance criteria 1-3 from the plan.
#[tokio::test]
async fn multi_device_sequential_adds_visible_after_pull() {
    let tmp = tempdir().unwrap();
    let backend_dir = tmp.path().join("backend");
    let vault_a = tmp.path().join("vault-a");
    let vault_b = tmp.path().join("vault-b");

    let (cfg_a, keys_a) = setup_primary_vault(&vault_a, &backend_dir).await;

    // A publishes a.txt
    let a_txt = vault_a.join("a.txt");
    fs::write(&a_txt, b"content from machine A").unwrap();
    sync_tree_and_push(&cfg_a, &keys_a, std::slice::from_ref(&a_txt)).await;

    // B opens and sees a.txt
    let (cfg_b, keys_b) = setup_secondary_vault(&vault_b, &backend_dir).await;
    let plain_b = cfg_b.load_plain_index(&keys_b).unwrap();
    assert_eq!(
        basenames(&plain_b),
        HashSet::from(["a.txt".into()]),
        "B after open should see A's file"
    );

    // B publishes b.txt
    let b_txt = vault_b.join("b.txt");
    fs::write(&b_txt, b"content from machine B").unwrap();
    sync_tree_and_push(&cfg_b, &keys_b, std::slice::from_ref(&b_txt)).await;

    // A pulls and sees a.txt + b.txt
    pull_indexes(&cfg_a, &keys_a).await;
    let plain_a = cfg_a.load_plain_index(&keys_a).unwrap();
    assert_eq!(
        basenames(&plain_a),
        HashSet::from(["a.txt".into(), "b.txt".into()]),
        "A after pull should see B's add"
    );

    // A publishes a2.txt (local index already merged remote via pull)
    let a2_txt = vault_a.join("a2.txt");
    fs::write(&a2_txt, b"second file from A").unwrap();
    sync_tree_and_push(&cfg_a, &keys_a, std::slice::from_ref(&a2_txt)).await;

    // B pulls and sees the full union
    pull_indexes(&cfg_b, &keys_b).await;
    let plain_b = cfg_b.load_plain_index(&keys_b).unwrap();
    assert_eq!(
        basenames(&plain_b),
        HashSet::from(["a.txt".into(), "b.txt".into(), "a2.txt".into()]),
        "B after pull should see A's second add"
    );
}

/// Concurrent multi-writer: both sides add without pulling each other first.
///
/// Desired: after both push and both pull, the index is the union of adds.
/// Current LWW full-index replace loses the first pusher's exclusive add.
/// This is the red test for multi-device index merge.
#[tokio::test]
async fn multi_device_concurrent_adds_preserve_union() {
    let tmp = tempdir().unwrap();
    let backend_dir = tmp.path().join("backend");
    let vault_a = tmp.path().join("vault-a");
    let vault_b = tmp.path().join("vault-b");

    let (cfg_a, keys_a) = setup_primary_vault(&vault_a, &backend_dir).await;

    // Shared baseline so both machines start from the same remote index.
    let base = vault_a.join("base.txt");
    fs::write(&base, b"shared baseline").unwrap();
    sync_tree_and_push(&cfg_a, &keys_a, std::slice::from_ref(&base)).await;

    let (cfg_b, keys_b) = setup_secondary_vault(&vault_b, &backend_dir).await;
    let plain_b = cfg_b.load_plain_index(&keys_b).unwrap();
    assert_eq!(basenames(&plain_b), HashSet::from(["base.txt".into()]));

    // Divergent adds without intermediate pull.
    let a_only = vault_a.join("a_only.txt");
    fs::write(&a_only, b"only on A").unwrap();
    sync_tree_and_push(&cfg_a, &keys_a, std::slice::from_ref(&a_only)).await;

    let b_only = vault_b.join("b_only.txt");
    fs::write(&b_only, b"only on B").unwrap();
    // B still has local index {base} only; merge-on-push keeps a_only.
    sync_tree_and_push(&cfg_b, &keys_b, std::slice::from_ref(&b_only)).await;

    // Both pull the merged remote index.
    pull_indexes(&cfg_a, &keys_a).await;
    pull_indexes(&cfg_b, &keys_b).await;

    let plain_a = cfg_a.load_plain_index(&keys_a).unwrap();
    let plain_b = cfg_b.load_plain_index(&keys_b).unwrap();
    let expected = HashSet::from(["base.txt".into(), "a_only.txt".into(), "b_only.txt".into()]);

    assert_eq!(
        basenames(&plain_a),
        expected,
        "A after concurrent push+pull should keep both machines' adds"
    );
    assert_eq!(
        basenames(&plain_b),
        expected,
        "B after concurrent push+pull should keep both machines' adds"
    );
}

/// A deletes a shared file and pushes; B still has it locally and pushes
/// without pulling first. After both pull, the tombstone must win so the
/// file does not reanimate on either side.
#[tokio::test]
async fn multi_device_delete_tombstone_propagates() {
    let tmp = tempdir().unwrap();
    let backend_dir = tmp.path().join("backend");
    let vault_a = tmp.path().join("vault-a");
    let vault_b = tmp.path().join("vault-b");

    let (cfg_a, keys_a) = setup_primary_vault(&vault_a, &backend_dir).await;

    let shared = vault_a.join("shared.txt");
    fs::write(&shared, b"shared content").unwrap();
    sync_tree_and_push(&cfg_a, &keys_a, std::slice::from_ref(&shared)).await;

    let (cfg_b, keys_b) = setup_secondary_vault(&vault_b, &backend_dir).await;
    assert_eq!(
        basenames(&cfg_b.load_plain_index(&keys_b).unwrap()),
        HashSet::from(["shared.txt".into()])
    );

    // A deletes and publishes the tombstone.
    let (plain_a, _blob_a) = delete_all_files(&cfg_a, &keys_a).await;
    assert!(plain_a.files_map_ref().is_empty());
    assert!(!plain_a.deleted_files_ref().is_empty());
    let backend = cfg_a.init_storage_backend().await.unwrap();
    cfg_a.merge_remote_indexes(&backend, &keys_a).await.unwrap();
    cfg_a.push_indexes(&backend).await.unwrap();

    // B never pulled the delete; still has shared.txt and pushes a new file.
    let b_extra = vault_b.join("b_extra.txt");
    fs::write(&b_extra, b"extra on B").unwrap();
    sync_tree_and_push(&cfg_b, &keys_b, std::slice::from_ref(&b_extra)).await;

    pull_indexes(&cfg_a, &keys_a).await;
    pull_indexes(&cfg_b, &keys_b).await;

    let plain_a = cfg_a.load_plain_index(&keys_a).unwrap();
    let plain_b = cfg_b.load_plain_index(&keys_b).unwrap();
    let expected = HashSet::from(["b_extra.txt".into()]);

    assert_eq!(
        basenames(&plain_a),
        expected,
        "A should keep B's add and not revive deleted shared.txt: {:?}",
        basenames(&plain_a)
    );
    assert_eq!(
        basenames(&plain_b),
        expected,
        "B should drop shared.txt after merging A's tombstone: {:?}",
        basenames(&plain_b)
    );
}
