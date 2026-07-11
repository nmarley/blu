use std::collections::{HashMap, HashSet};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glob::Pattern;
use indicatif::{ProgressBar, ProgressStyle};
use multihash::Multihash;
use sha2::{Digest, Sha512};
use tokio::sync::{mpsc, Semaphore};

use crate::blob::BlobBlockLocation;
use crate::block::ChunkMeta;
use crate::cli::clapargs::RestoreArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::compression::decompress;
use crate::dek_provider::{decrypt_envelope, decrypt_envelope_segmented_prefix, DekProvider};
use crate::error::BluError;
use crate::format::human_bytes;
use crate::hash::{self, Hash, SHA2_512};
use crate::storage;
use crate::v3format;

/// Progress event sent from prefetch workers to the progress consumer.
enum PrefetchEvent {
    /// A blob was fetched, decrypted, and cached.
    Fetched {
        blob_hash: Hash,
        data: Vec<u8>,
        bytes: u64,
    },
    /// A blob fetch or decrypt failed.
    Failed(String),
}

/// Materialize plaintext files from the catalog and encrypted blobs.
pub async fn restore(args: RestoreArgs) -> Result<(), BluError> {
    info!("Started restore");

    // Validate arguments
    if args.file_hashes.is_empty() && args.path.is_none() && !args.all {
        return Err(BluError::Internal(
            "Must specify --file-hashes, --path, or --all".into(),
        ));
    }

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;
    let plain_index = cfg.load_plain_index(&keys)?;
    let blob_index = cfg.load_blob_index_or_default(&keys);
    let files_map = plain_index.files_map_ref();

    let backend = match &args.backend {
        Some(name) => cfg.init_named_backend(name).await?,
        None => cfg.init_storage_backend().await?,
    };

    // Build path pattern matcher if specified
    let path_pattern = match args.path.as_ref() {
        Some(p) => match Pattern::new(p) {
            Ok(pat) => Some(pat),
            Err(e) => {
                warn!("Invalid glob pattern '{}': {}, treating as literal", p, e);
                Some(Pattern::new(&glob::Pattern::escape(p)).map_err(|e| {
                    BluError::Internal(format!("failed to escape glob pattern '{}': {}", p, e))
                })?)
            }
        },
        None => None,
    };

    // Collect files to restore
    let mut unique_hashes: HashSet<Hash> = HashSet::new();

    for (hash, fileref) in files_map.iter() {
        let mut should_restore = false;

        // Check if --all
        if args.all {
            should_restore = true;
        }

        // Check if hash matches any provided hash prefix
        if !args.file_hashes.is_empty() {
            let hash_str = hash.to_string();
            for h in &args.file_hashes {
                if hash_str.contains(h) {
                    println!("Got a match on file hash: {}", hash.dbg_short(9));
                    should_restore = true;
                    break;
                }
            }
        }

        // Check if any path matches the pattern
        if let Some(ref pattern) = path_pattern {
            for path in &fileref.paths {
                if pattern.matches_path(path) {
                    println!("Got a match on path: {}", path.display());
                    should_restore = true;
                    break;
                }
            }
        }

        if should_restore {
            unique_hashes.insert(hash.clone());
        }
    }

    if unique_hashes.is_empty() {
        println!("No files matched the specified criteria");
        return Ok(());
    }

    println!("Found {} file(s) to restore", unique_hashes.len());

    // Collect all unique blob paths needed for the restore
    let mut needed_blob_paths: HashMap<Hash, PathBuf> = HashMap::new();

    for file_hash in &unique_hashes {
        let fileref = match plain_index.get_fileref_ref(file_hash) {
            Some(fileref) => fileref,
            None => continue,
        };

        for chunkmeta in &fileref.chunkmetas {
            if !blob_index.has_chunk(&chunkmeta.hash) {
                continue;
            }
            if let Ok(location) = blob_index.get_block_location_ref(&chunkmeta.hash) {
                let blob_hash = storage::hash_from_path(location.blob_path())?;
                needed_blob_paths
                    .entry(blob_hash)
                    .or_insert_with(|| location.blob_path().clone());
            }
        }
    }

    let total_blobs = needed_blob_paths.len();
    println!("Prefetching {} blob(s)...", total_blobs);

    // Prefetch all blobs concurrently
    let blob_cache = prefetch_blobs(
        needed_blob_paths,
        &backend,
        &keys,
        16, // concurrency
    )
    .await?;

    // Parse destination directory
    let dest_dir = args.to.as_ref().map(PathBuf::from);

    // Restore files using the prefetched cache
    'outer: for file_hash in unique_hashes.into_iter() {
        let fileref = match plain_index.get_fileref_ref(&file_hash) {
            Some(fileref) => fileref,
            None => {
                eprintln!(
                    "Unable to restore file: File hash not found in plain index: {:?}",
                    file_hash
                );
                continue; // next file
            }
        };

        let file_size = fileref.total_size();
        println!(
            "Restoring {} ({}, {} chunks)",
            file_hash.dbg_short(9),
            human_bytes(file_size),
            fileref.chunkmetas.len(),
        );

        // Determine restore path(s) based on --to option.
        // In both modes, the first path gets the data and the rest
        // are hard-linked to it (deduplication).
        let (restore_path, other_paths): (PathBuf, Vec<PathBuf>) = {
            let mut path_iter = fileref.paths.iter();
            let first_orig = match path_iter.next() {
                Some(p) => p,
                None => {
                    eprintln!(
                        "Unable to restore file: no paths recorded for hash {:?}",
                        file_hash
                    );
                    continue 'outer;
                }
            };

            if let Some(ref dest) = dest_dir {
                // --to mode: preserve relative directory structure
                let first = Path::new(dest).join(first_orig);
                let others = path_iter
                    .map(|p| Path::new(dest).join(p))
                    .collect::<Vec<_>>();
                (first, others)
            } else {
                // Restore to original paths
                let others = path_iter.cloned().collect::<Vec<_>>();
                (first_orig.clone(), others)
            }
        };

        // Print all original paths
        for path in fileref.paths.iter() {
            println!("  {}", path.display());
        }

        // Check if destination file exists
        if restore_path.exists() {
            eprintln!(
                "Unable to restore file: There already exists a file at: {:?}",
                restore_path
            );
            continue 'outer;
        }

        // Check other paths too (only in non --to mode)
        for other in &other_paths {
            if other.exists() {
                eprintln!(
                    "Unable to restore file: There already exists a file at: {:?}",
                    other
                );
                continue 'outer;
            }
        }

        // Fail closed before creating any dest file: every chunk must
        // have ciphertext in the blob index.
        if let Some(missing) = fileref
            .chunkmetas
            .iter()
            .find(|cm| !blob_index.has_chunk(&cm.hash))
        {
            eprintln!(
                "Unable to restore file {}: chunk {} has no ciphertext in the blob index",
                file_hash.dbg_short(9),
                missing.hash.dbg_short(9),
            );
            continue 'outer;
        }

        println!("  -> {}", restore_path.display());

        // Create parent directories if needed
        if let Some(parent) = restore_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        // Create a sparse file of the correct size
        let fh = match std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&restore_path)
        {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Unable to create {:?}: {}", restore_path, e);
                continue 'outer;
            }
        };
        if let Err(e) = fh.set_len(file_size) {
            let _ = std::fs::remove_file(&restore_path);
            return Err(BluError::Io(e));
        }

        let started = Instant::now();
        let mut offset = 0u64;
        let total_chunks = fileref.chunkmetas.len();
        let mut file_hasher = Sha512::new();

        let write_result: Result<(), BluError> = (|| {
            for (i, chunkmeta) in fileref.chunkmetas.iter().enumerate() {
                let location = blob_index.get_block_location_ref(&chunkmeta.hash)?;
                debug!(
                    "chunk {}/{}: hash={}, offset={}, size={}",
                    i + 1,
                    total_chunks,
                    chunkmeta.hash.dbg_short(9),
                    location.position.offset,
                    location.position.size,
                );

                let block_data = get_cached_bytes(&blob_cache, &location)?;
                verify_chunk_bytes(block_data, chunkmeta)?;
                fh.write_all_at(block_data, offset)?;
                file_hasher.update(block_data);
                trace!(
                    "wrote {} bytes at offset {} to {:?}",
                    block_data.len(),
                    offset,
                    restore_path,
                );
                offset += block_data.len() as u64;
            }

            if offset != file_size {
                return Err(BluError::Internal(format!(
                    "restored size mismatch for {}: wrote {} bytes, catalog size {}",
                    file_hash.dbg_short(9),
                    offset,
                    file_size
                )));
            }

            let file_mh: Multihash<64> = Multihash::wrap(SHA2_512, &file_hasher.finalize())
                .map_err(|e| BluError::Internal(format!("file multihash wrap: {}", e)))?;
            let actual = Hash::from(file_mh.to_bytes());
            if actual != file_hash {
                return Err(BluError::Internal(format!(
                    "file hash mismatch: expected {}, got {}",
                    file_hash, actual
                )));
            }
            Ok(())
        })();

        if let Err(e) = write_result {
            let _ = std::fs::remove_file(&restore_path);
            eprintln!(
                "Unable to restore {}: {}; removed partial file",
                restore_path.display(),
                e
            );
            return Err(e);
        }

        let elapsed = started.elapsed();
        let rate = if elapsed.as_secs_f64() > 0.0 {
            file_size as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };
        println!(
            "  restored {} in {:.2}s ({}/s)",
            human_bytes(file_size),
            elapsed.as_secs_f64(),
            human_bytes(rate as u64),
        );

        // hard links for the same data with multiple filenames
        for other in &other_paths {
            // Create parent directories if needed
            if let Some(parent) = other.parent() {
                if !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!(
                            "Unable to create parent dir for hard link {:?}: {:?}",
                            other, e
                        );
                        continue;
                    }
                }
            }
            match std::fs::hard_link(&restore_path, other) {
                Ok(_) => {
                    println!("Created hard link: {:?}", other);
                }
                Err(e) => {
                    eprintln!("Unable to create hard link {:?}: {:?}", other, e);
                }
            }
        }
    }

    Ok(())
}

/// Prefetch all needed blobs concurrently, returning a cache of
/// blob_hash -> decrypted, decompressed blob data.
async fn prefetch_blobs(
    needed: HashMap<Hash, PathBuf>,
    backend: &storage::BackendKind,
    keys: &DekProvider,
    concurrency: usize,
) -> Result<HashMap<Hash, Vec<u8>>, BluError> {
    let total = needed.len();
    if total == 0 {
        return Ok(HashMap::new());
    }

    let semaphore = Arc::new(Semaphore::new(concurrency));
    let (tx, mut rx) = mpsc::channel::<PrefetchEvent>(concurrency * 4);

    // Spawn worker tasks
    let workers = tokio::spawn({
        let backend = backend.clone();
        let keys = keys.clone();
        async move {
            let mut tasks = tokio::task::JoinSet::new();

            for (blob_hash, blob_path) in needed {
                let sem = Arc::clone(&semaphore);
                let be = backend.clone();
                let k = keys.clone();
                let tx = tx.clone();

                tasks.spawn(async move {
                    let _permit = sem.acquire().await.expect("semaphore closed");

                    let raw = be
                        .read_data(&blob_path)
                        .await
                        .map_err(|e| format!("error reading blob {}: {}", blob_path.display(), e));

                    let raw = match raw {
                        Ok(data) => data,
                        Err(msg) => {
                            let _ = tx.send(PrefetchEvent::Failed(msg)).await;
                            return;
                        }
                    };

                    let bytes = raw.len() as u64;

                    let decompressed = match decrypt_blob_to_plaintext(&raw, &k) {
                        Ok(d) => d,
                        Err(e) => {
                            let msg =
                                format!("error decrypting blob {}: {}", blob_path.display(), e);
                            let _ = tx.send(PrefetchEvent::Failed(msg)).await;
                            return;
                        }
                    };

                    let _ = tx
                        .send(PrefetchEvent::Fetched {
                            blob_hash,
                            data: decompressed,
                            bytes,
                        })
                        .await;
                });
            }

            drop(tx);
            while tasks.join_next().await.is_some() {}
        }
    });

    // Progress bar consumer
    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{bar:40} {pos}/{len} [{elapsed_precise}] {msg}")
            .expect("valid progress bar template"),
    );

    let mut cache: HashMap<Hash, Vec<u8>> = HashMap::with_capacity(total);
    let mut fetched = 0u64;
    let mut failed = 0u64;
    let mut bytes_total = 0u64;

    while let Some(event) = rx.recv().await {
        match event {
            PrefetchEvent::Fetched {
                blob_hash,
                data,
                bytes,
            } => {
                fetched += 1;
                bytes_total += bytes;
                cache.insert(blob_hash, data);
                pb.inc(1);
            }
            PrefetchEvent::Failed(msg) => {
                pb.suspend(|| eprintln!("  {}", msg));
                failed += 1;
                pb.inc(1);
            }
        }

        pb.set_message(format!(
            "{} fetched, {} failed, {}",
            fetched,
            failed,
            human_bytes(bytes_total),
        ));
    }

    pb.finish_and_clear();
    workers.await?;

    println!(
        "Prefetch complete: {} blobs ({})",
        fetched,
        human_bytes(bytes_total),
    );

    if failed > 0 {
        return Err(BluError::StorageError(format!(
            "{} blob(s) failed to fetch",
            failed
        )));
    }

    Ok(cache)
}

/// Look up chunk data from the prefetched blob cache.
fn get_cached_bytes<'a>(
    cache: &'a HashMap<Hash, Vec<u8>>,
    location: &BlobBlockLocation,
) -> Result<&'a [u8], BluError> {
    let blob_hash = storage::hash_from_path(location.blob_path())?;
    let full_data = cache.get(&blob_hash).ok_or_else(|| {
        BluError::Internal(format!(
            "blob not in cache: {}",
            location.blob_path().display()
        ))
    })?;
    let pos = &location.position;
    let end = pos.offset.checked_add(pos.size).ok_or_else(|| {
        BluError::Internal(format!(
            "chunk slice overflow in blob {}: offset={} size={}",
            location.blob_path().display(),
            pos.offset,
            pos.size
        ))
    })?;
    if end > full_data.len() {
        return Err(BluError::Internal(format!(
            "chunk slice out of bounds in blob {}: offset={} size={} blob_len={}",
            location.blob_path().display(),
            pos.offset,
            pos.size,
            full_data.len()
        )));
    }
    Ok(&full_data[pos.offset..end])
}

/// Verify restored chunk bytes match catalog size and multihash.
fn verify_chunk_bytes(data: &[u8], chunkmeta: &ChunkMeta) -> Result<(), BluError> {
    if data.len() != chunkmeta.size {
        return Err(BluError::Internal(format!(
            "chunk size mismatch: expected {}, got {}",
            chunkmeta.size,
            data.len()
        )));
    }
    let actual = Hash::from(hash::multihash(data).to_bytes());
    if actual != chunkmeta.hash {
        return Err(BluError::BlockHashMismatch {
            expected: chunkmeta.hash.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}

/// Decrypt and decompress a whole blob file into plaintext chunk packing.
///
/// Handles both v2 (single AEAD box) and v3 (segmented AEAD). Matches
/// the full-blob path used by [`crate::blob::EncBlobReader`] so restore
/// can open vaults written by current sync/encrypt.
fn decrypt_blob_to_plaintext(raw: &[u8], keys: &DekProvider) -> Result<Vec<u8>, BluError> {
    match v3format::peek_version(raw) {
        Some(v3format::FORMAT_VERSION_V3) => {
            let (header, _) = v3format::read_header(raw)?;
            let last_seg = header.segment_count.saturating_sub(1);
            decrypt_envelope_segmented_prefix(raw, last_seg, keys)
        }
        _ => {
            let decrypted = decrypt_envelope(raw, keys)?;
            decompress(&decrypted).map_err(|e| {
                BluError::DecryptionFailed(format!("decompress after v2 decrypt: {}", e))
            })
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::blob::BlobBlockLocation;
    use crate::compression::compress;
    use crate::dek_provider::{encrypt_envelope, encrypt_envelope_segmented};
    use crate::io::Position;
    use crate::keys::kek::Kek;
    use crate::v2format::FileType;
    use std::path::PathBuf;

    fn local_keys() -> DekProvider {
        DekProvider::Local {
            kek: Kek::generate(),
            kek_version: 0,
        }
    }

    #[test]
    fn decrypt_blob_to_plaintext_v2() {
        let keys = local_keys();
        let plain = b"hello v2 restore path";
        let compressed = compress(plain).unwrap();
        let raw = encrypt_envelope(&compressed, FileType::Blob, &keys).unwrap();
        assert_eq!(v3format::peek_version(&raw), Some(2));

        let out = decrypt_blob_to_plaintext(&raw, &keys).unwrap();
        assert_eq!(out, plain);
    }

    #[test]
    fn decrypt_blob_to_plaintext_v3() {
        let keys = local_keys();
        let plain = b"hello v3 restore path that is a bit longer for segments";
        let compressed = compress(plain).unwrap();
        let raw = encrypt_envelope_segmented(&compressed, 64, &keys).unwrap();
        assert_eq!(
            v3format::peek_version(&raw),
            Some(v3format::FORMAT_VERSION_V3)
        );

        let out = decrypt_blob_to_plaintext(&raw, &keys).unwrap();
        assert_eq!(out, plain);
    }

    #[test]
    fn decrypt_blob_to_plaintext_v3_rejects_wrong_kek() {
        let keys_write = local_keys();
        let keys_read = local_keys();
        let compressed = compress(b"secret").unwrap();
        let raw = encrypt_envelope_segmented(&compressed, 64, &keys_write).unwrap();

        let err = decrypt_blob_to_plaintext(&raw, &keys_read).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("decrypt")
                || err.to_string().to_lowercase().contains("fail"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn verify_chunk_bytes_accepts_matching_data() {
        let data = b"chunk body";
        let cm = ChunkMeta::new(data);
        verify_chunk_bytes(data, &cm).unwrap();
    }

    #[test]
    fn verify_chunk_bytes_rejects_size_and_hash_mismatch() {
        let cm = ChunkMeta::new(b"expected");
        let size_err = verify_chunk_bytes(b"short", &cm).unwrap_err();
        assert!(
            size_err.to_string().contains("chunk size mismatch"),
            "{size_err}"
        );

        let hash_err = verify_chunk_bytes(
            b"expected!",
            &ChunkMeta {
                hash: cm.hash.clone(),
                size: 9,
            },
        )
        .unwrap_err();
        assert!(
            hash_err.to_string().contains("block hash mismatch"),
            "{hash_err}"
        );
    }

    #[test]
    fn get_cached_bytes_rejects_out_of_bounds_slice() {
        let hash_hex = "1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6";
        let blob_path = PathBuf::from(format!("d/dd4/dd4ce/{hash_hex}"));
        let blob_hash = storage::hash_from_path(&blob_path).unwrap();
        let mut cache = HashMap::new();
        cache.insert(blob_hash, vec![0u8; 8]);
        let location = BlobBlockLocation::new(
            blob_path,
            Position {
                offset: 4,
                size: 16,
            },
        );
        let err = get_cached_bytes(&cache, &location).unwrap_err();
        assert!(
            err.to_string().contains("out of bounds"),
            "unexpected error: {err}"
        );
    }
}
