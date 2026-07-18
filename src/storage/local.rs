//! Local filesystem storage backend implementation.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};

use crate::error::BluError;
use crate::hash::Hash;

/// Local storage backend for managing data on a local filesystem.
///
/// Tracks a cumulative count of bytes returned by read operations as an
/// observability hook. The counter is shared across clones (it lives
/// behind an `Arc`), so a cloned backend handed to a reader still
/// reports its fetches through the original. It defaults to zero and
/// costs a single relaxed atomic add per read.
#[derive(Default, Debug, Clone)]
pub struct Local {
    datadir: PathBuf,
    bytes_read: Arc<AtomicU64>,
}

impl Local {
    /// Create a new Local storage backend with the given datadir.
    pub fn new<P: AsRef<Path>>(datadir: P) -> Self {
        Self {
            datadir: datadir.as_ref().to_path_buf(),
            bytes_read: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Total bytes returned by `read_data` and `read_range` on this
    /// backend (and its clones) since creation. Used to measure the v3
    /// prefix-fetch win against a v2 whole-blob read.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    /// Absolute path of the on-disk data directory. Exposed so tests
    /// can resolve content-addressed blob paths and assert on
    /// filesystem metadata (e.g., that an overwrite did not
    /// delete-and-recreate an identical blob).
    pub fn datadir(&self) -> &Path {
        &self.datadir
    }

    /// Read the data blob at the given relative path from local disk.
    ///
    /// The path should be a relative content-addressed path (e.g.,
    /// `blobs/d/dd4/dd4ce/dd4ce38e...`). The backend prepends
    /// `self.datadir` to resolve the full filesystem path.
    pub async fn read_data(&self, path: &Path) -> Result<Vec<u8>, BluError> {
        let full_path = self.datadir.join(path);
        let data = tokio::fs::read(&full_path).await?;
        self.bytes_read
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        Ok(data)
    }

    /// Read the byte range `[start, end)` (end exclusive) of the file
    /// at the given relative path.
    ///
    /// Seeks to `start` and reads up to `end - start` bytes. `end` is
    /// clamped to EOF by the underlying reader, so a window past the
    /// file length returns the available tail. A `start` past EOF
    /// yields an empty vector.
    pub async fn read_range(&self, path: &Path, start: u64, end: u64) -> Result<Vec<u8>, BluError> {
        let full_path = self.datadir.join(path);
        let mut file = tokio::fs::File::open(&full_path).await?;
        file.seek(SeekFrom::Start(start)).await?;

        let len = end.saturating_sub(start);
        let mut buf = Vec::new();
        file.take(len).read_to_end(&mut buf).await?;
        self.bytes_read
            .fetch_add(buf.len() as u64, Ordering::Relaxed);
        Ok(buf)
    }

    /// Write data to a content-addressed path derived from the hash.
    ///
    /// Returns the relative content-addressed path (e.g.,
    /// `blobs/d/dd4/dd4ce/dd4ce38e...`), consistent with
    /// `AmazonS3::write_data`.
    pub async fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, BluError> {
        let hash_path = super::path_for(hash)?;
        let full_path = self.datadir.join(&hash_path);

        let parent = full_path.parent().ok_or_else(|| {
            BluError::Internal(format!("path has no parent: {}", full_path.display()))
        })?;
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::write(&full_path, data).await?;
        Ok(hash_path)
    }

    /// Check if a blob exists at the given relative path.
    ///
    /// The path should be a relative content-addressed path. The backend
    /// prepends `self.datadir` to resolve the full filesystem path.
    pub async fn exists(&self, path: &Path) -> Result<bool, BluError> {
        let full_path = self.datadir.join(path);
        Ok(tokio::fs::try_exists(&full_path).await?)
    }

    /// Delete a blob at the given relative path.
    ///
    /// The path should be a relative content-addressed path. The backend
    /// prepends `self.datadir` to resolve the full filesystem path.
    pub async fn delete(&self, path: &Path) -> Result<(), BluError> {
        let full_path = self.datadir.join(path);
        tokio::fs::remove_file(&full_path).await?;
        Ok(())
    }

    /// Write data to a known path in the backend (not hash-derived).
    pub async fn write_to_path(&self, path: &Path, data: &[u8]) -> Result<(), BluError> {
        let full_path = self.datadir.join(path);
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&full_path, data).await?;
        Ok(())
    }

    /// Read data from a known path in the backend (not hash-derived).
    pub async fn read_from_path(&self, path: &Path) -> Result<Vec<u8>, BluError> {
        let full_path = self.datadir.join(path);
        let data = tokio::fs::read(&full_path).await?;
        Ok(data)
    }

    /// List relative paths of content-addressed blob files under datadir.
    ///
    /// Walks only the `blobs/` subtree, so catalog material (`indexes/`,
    /// `keys/`) is never listed. Missing datadir or `blobs/` dir yields
    /// an empty list (not an error).
    pub async fn list_blob_paths(&self) -> Result<Vec<PathBuf>, BluError> {
        let datadir = self.datadir.clone();
        tokio::task::spawn_blocking(move || list_blob_paths_sync(&datadir))
            .await
            .map_err(|e| BluError::Internal(format!("list_blob_paths join: {}", e)))?
    }

    /// Local files are always immediately available.
    pub async fn stat_object(&self, path: &Path) -> Result<super::ObjectStat, BluError> {
        let full_path = self.datadir.join(path);
        let meta = tokio::fs::metadata(&full_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BluError::StorageFileNotFound {
                    path: path.to_path_buf(),
                }
            } else {
                BluError::Io(e)
            }
        })?;
        Ok(super::ObjectStat {
            path: path.to_path_buf(),
            storage_class: None,
            archive_status: None,
            availability: super::ObjectAvailability::Available,
            restore_header: None,
            content_length: Some(meta.len()),
        })
    }

    /// No-op: local objects are never archived.
    pub async fn restore_object(
        &self,
        _path: &Path,
        _prior: Option<&super::ObjectStat>,
        _opts: &super::RestoreOptions,
    ) -> Result<(), BluError> {
        Ok(())
    }
}

fn list_blob_paths_sync(datadir: &Path) -> Result<Vec<PathBuf>, BluError> {
    let blob_root = datadir.join(super::BLOB_PREFIX);
    if !blob_root.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    let mut stack = vec![blob_root];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| BluError::Internal(format!("read_dir {}: {}", dir.display(), e)))?;
        for entry in entries {
            let entry = entry.map_err(|e| {
                BluError::Internal(format!("read_dir entry under {}: {}", dir.display(), e))
            })?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| BluError::Internal(format!("file_type {}: {}", path.display(), e)))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            // Strip datadir (not blob_root) so relative paths keep the
            // blobs/ component and match `path_for` output.
            let rel = path.strip_prefix(datadir).map_err(|e| {
                BluError::Internal(format!(
                    "strip_prefix {} from {}: {}",
                    datadir.display(),
                    path.display(),
                    e
                ))
            })?;
            out.push(rel.to_path_buf());
        }
    }
    out.sort();
    Ok(out)
}
