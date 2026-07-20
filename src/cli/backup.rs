//! Backup command - index paths, encrypt, and publish to the backend.

use std::path::Path;
use std::sync::Arc;

use itertools::Itertools;
use tokio::sync::mpsc;

use crate::blob::{BlobBuffer, BlobBufferEvent};
use crate::block::IndexReporter;
use crate::cli::clapargs::BackupArgs;
use crate::cli::helpers::{load_config_and_keys, push_indexes_or_fail, LoadOptions};
use crate::cli::progress::{
    apply_backup_event, should_draw_progress, BackupEvent, BackupPhase, BackupProgress,
    BackupUiState, ChannelProgress, NullProgress, ProgressUi,
};
use crate::error::BluError;
use crate::hash::{self, Hash};
use crate::ignore::walk_files_with_sizes;

/// Publish local files into the encrypted vault.
///
/// This command performs the following steps:
/// 1. Adds all files from the specified paths to the index
/// 2. Encrypts any chunks not yet encrypted
/// 3. Writes the updated indexes
/// 4. Merges remote indexes and pushes the catalog to the backend
///
/// It is idempotent and safe to run repeatedly. Progress bars are shown by
/// default on a TTY; pass `--quiet` to suppress them.
pub async fn backup(args: BackupArgs) -> Result<(), BluError> {
    info!("Started backup");

    let draw = should_draw_progress(args.quiet);
    let (progress, ui_task): (Arc<dyn BackupProgress>, Option<tokio::task::JoinHandle<()>>) =
        if draw {
            let (sink, rx) = ChannelProgress::channel();
            let handle = tokio::spawn(async move {
                run_backup_ui(rx).await;
            });
            (Arc::new(sink), Some(handle))
        } else {
            (Arc::new(NullProgress), None)
        };

    let result = run_backup(args, Arc::clone(&progress)).await;

    // Drop the sink so the UI consumer exits, then clear bars before
    // returning (including on error).
    drop(progress);
    if let Some(handle) = ui_task {
        let _ = handle.await;
    }

    result
}

async fn run_backup(args: BackupArgs, progress: Arc<dyn BackupProgress>) -> Result<(), BluError> {
    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    let mut plain_index = cfg.load_plain_index_or_default(&keys);

    let paths_to_add = if args.paths.is_empty() {
        vec![".".to_string()]
    } else {
        args.paths.clone()
    };

    // Pre-scan for overall index work units.
    let (index_files, index_bytes) = plan_index_work(&paths_to_add);
    progress.emit(BackupEvent::Phase(BackupPhase::Index));
    progress.emit(BackupEvent::IndexPlan {
        files: index_files,
        bytes: index_bytes,
    });

    let mut files_added = 0;
    let mut reporter = ProgressIndexReporter {
        progress: Arc::clone(&progress),
    };
    for p in &paths_to_add {
        info!("Adding {:?}", p);
        let before_count = plain_index.files_map_ref().len();
        plain_index.add_with_reporter(p.clone(), None, &mut reporter)?;
        let after_count = plain_index.files_map_ref().len();
        files_added += after_count.saturating_sub(before_count);
    }

    cfg.write_plain_index(&plain_index, &keys)?;

    let mut blob_index = cfg.load_blob_index_or_default(&keys);
    let backend = match &args.backend {
        Some(name) => cfg.init_named_backend(name).await?,
        None => cfg.init_storage_backend().await?,
    };

    let (missing_chunks, missing_bytes) = plan_encrypt_work(&plain_index, &blob_index);
    progress.emit(BackupEvent::Phase(BackupPhase::EncryptUpload));
    progress.emit(BackupEvent::EncryptPlan {
        chunks: missing_chunks,
        bytes: missing_bytes,
    });

    let mut blob_buf = BlobBuffer::new(&backend, keys.clone());

    // Forward blob seal/upload telemetry into the backup event sink.
    let (blob_tx, blob_rx) = mpsc::channel::<BlobBufferEvent>(256);
    blob_buf.set_event_sender(blob_tx);
    let blob_forward = spawn_blob_event_forwarder(blob_rx, Arc::clone(&progress));

    let mut chunks_encrypted = 0u64;
    let files_map = plain_index.files_map_ref();
    let file_hashes = files_map.keys().clone().sorted_unstable();

    for file_hash in file_hashes {
        let file_ref = files_map
            .get(file_hash)
            .ok_or_else(|| BluError::FileHashNotFound {
                hash: file_hash.to_string(),
            })?;

        for cm in &file_ref.chunkmetas {
            if blob_index.has_chunk(&cm.hash) {
                continue;
            }

            let block_ref = plain_index.blocks_map_ref().get(&cm.hash).ok_or_else(|| {
                BluError::BlockNotFound {
                    hash: cm.hash.to_string(),
                }
            })?;
            let data = plain_index.read_block_bytes(block_ref)?;

            let block_hash2 = Hash::from(hash::multihash(&data).to_bytes());
            if cm.hash != block_hash2 {
                return Err(BluError::BlockHashMismatch {
                    expected: cm.hash.to_string(),
                    actual: block_hash2.to_string(),
                });
            }

            let chunk_bytes = data.len() as u64;
            blob_buf
                .add_chunk(&mut data.clone(), &mut blob_index)
                .await?;
            progress.emit(BackupEvent::ChunkSealed { bytes: chunk_bytes });
            chunks_encrypted += 1;
        }
    }

    if chunks_encrypted > 0 || args.force {
        progress.emit(BackupEvent::Phase(BackupPhase::Finalize));
        blob_buf.finalize(&mut blob_index).await?;
        cfg.write_blob_index(&blob_index, &keys)?;
    }

    // Close blob event sender (held by blob_buf) and wait for forwarder.
    drop(blob_buf);
    let _ = blob_forward.await;

    progress.emit(BackupEvent::Phase(BackupPhase::PushIndexes));
    push_indexes_or_fail(&cfg, &keys, args.backend.as_deref(), Some(&backend)).await?;
    progress.emit(BackupEvent::PushIndexesFinished);

    println!(
        "Backup complete: {} files indexed, {} chunks encrypted",
        files_added, chunks_encrypted
    );
    println!("Index contains {} files total", files_map.len());
    println!(
        "Blob index contains {} blob files",
        blob_index.count_blob_files()
    );

    Ok(())
}

async fn run_backup_ui(mut rx: mpsc::Receiver<BackupEvent>) {
    let mut ui = ProgressUi::new(false);
    let mut state = BackupUiState::default();
    while let Some(event) = rx.recv().await {
        apply_backup_event(&mut ui, &mut state, event);
    }
    ui.finish();
}

fn spawn_blob_event_forwarder(
    mut rx: mpsc::Receiver<BlobBufferEvent>,
    progress: Arc<dyn BackupProgress>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            progress.emit(map_blob_event(event));
        }
    })
}

fn map_blob_event(event: BlobBufferEvent) -> BackupEvent {
    match event {
        BlobBufferEvent::Sealed { short_id, bytes } => BackupEvent::BlobSealed { short_id, bytes },
        BlobBufferEvent::Uploaded { short_id, bytes } => {
            BackupEvent::BlobUploaded { short_id, bytes }
        }
        BlobBufferEvent::UploadFailed { short_id, error } => {
            BackupEvent::BlobUploadFailed { short_id, error }
        }
    }
}

/// IndexReporter adapter that emits backup progress events.
struct ProgressIndexReporter {
    progress: Arc<dyn BackupProgress>,
}

impl IndexReporter for ProgressIndexReporter {
    fn on_file_start(&mut self, path: &Path, len: u64) {
        self.progress.emit(BackupEvent::IndexFileStarted {
            path: path.to_path_buf(),
            bytes: len,
        });
    }

    fn on_file_bytes(&mut self, n: u64) {
        self.progress
            .emit(BackupEvent::IndexFileProgress { bytes_delta: n });
    }

    fn on_file_end(&mut self, _path: &Path) {
        self.progress.emit(BackupEvent::IndexFileFinished);
    }
}

/// Count files and bytes that indexing will hash for the given roots.
fn plan_index_work(paths: &[String]) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    for p in paths {
        let path = Path::new(p);
        if path.is_file() {
            files += 1;
            bytes += std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        } else if path.is_dir() {
            for (_path, len) in walk_files_with_sizes(path) {
                files += 1;
                bytes += len;
            }
        }
    }
    (files, bytes)
}

/// Count missing chunks and their plaintext sizes for the encrypt phase.
fn plan_encrypt_work(
    plain_index: &crate::block::PlainIndex,
    blob_index: &crate::blob::BlobIndex,
) -> (u64, u64) {
    let mut chunks = 0u64;
    let mut bytes = 0u64;
    for file_ref in plain_index.files_map_ref().values() {
        for cm in &file_ref.chunkmetas {
            if blob_index.has_chunk(&cm.hash) {
                continue;
            }
            chunks += 1;
            bytes += cm.size as u64;
        }
    }
    (chunks, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_blob_event_preserves_ids() {
        let sealed = map_blob_event(BlobBufferEvent::Sealed {
            short_id: "abc".into(),
            bytes: 9,
        });
        match sealed {
            BackupEvent::BlobSealed { short_id, bytes } => {
                assert_eq!(short_id, "abc");
                assert_eq!(bytes, 9);
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn plan_index_work_empty_missing_path() {
        let (files, bytes) = plan_index_work(&["/no/such/path/for-blu-backup".into()]);
        assert_eq!(files, 0);
        assert_eq!(bytes, 0);
    }

    #[test]
    fn plan_index_work_single_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"hello-backup").unwrap();
        let path = tmp.path().to_string_lossy().into_owned();
        let (files, bytes) = plan_index_work(&[path]);
        assert_eq!(files, 1);
        assert_eq!(bytes, 12);
    }
}
