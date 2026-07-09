//! End-to-end smoke tests for the main vault data pipeline.
//!
//! Exercises library APIs that back the CLI (init → sync → list/status →
//! restore → delete → doctor) without requiring the agent daemon.

#![cfg(test)]

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use age::Identity;
use tempfile::tempdir;

use crate::blob::{BlobBuffer, BlobIndex, EncBlobReader};
use crate::block::PlainIndex;
use crate::cli::doctor::{diagnose, CheckStatus};
use crate::cli::{init_vault, InitVaultParams};
use crate::config::{self, Config};
use crate::dek_provider::DekProvider;
use crate::hash::Hash;
use crate::keys::kek::KekStore;
use crate::keys::mnemonic;
use crate::storage::BackendKind;

// Serialize tests that touch shared process state.
static SMOKE_LOCK: Mutex<()> = Mutex::new(());

const TEST_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon \
                              abandon abandon abandon abandon abandon abandon \
                              abandon abandon abandon abandon abandon abandon \
                              abandon abandon abandon abandon abandon art";

fn test_pq_recipient() -> String {
    let m = mnemonic::parse_mnemonic(TEST_MNEMONIC).unwrap();
    let seed = mnemonic::mnemonic_to_seed(&m, "");
    mnemonic::derive_pq_recipient(&seed).unwrap().to_string()
}

fn local_keys(cfg: &Config) -> DekProvider {
    let store = KekStore::new(&cfg.bludir());
    let m = mnemonic::parse_mnemonic(TEST_MNEMONIC).unwrap();
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

        plain.files.remove(file_hash);
        for chunk_hash in &chunk_hashes {
            let unreferenced = match plain.blocks.get_mut(chunk_hash) {
                Some(br) => br.delete_fileref(file_hash),
                None => false,
            };
            if unreferenced {
                plain.blocks.remove(chunk_hash);
                if blob.has_chunk(chunk_hash) {
                    blob.delete_chunk(chunk_hash).unwrap();
                }
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
    let _guard = SMOKE_LOCK.lock().unwrap();
    let (tmp, cfg, keys) = setup_vault();

    // Small tree to sync
    let docs = tmp.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    let a = docs.join("a.txt");
    let b = docs.join("b.txt");
    fs::write(&a, b"alpha content for smoke").unwrap();
    fs::write(&b, b"bravo content for smoke test").unwrap();

    // sync
    let (plain, blob) = sync_tree(&cfg, &keys, &[docs.clone()]).await;
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
    let _guard = SMOKE_LOCK.lock().unwrap();
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
    let _guard = SMOKE_LOCK.lock().unwrap();
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
