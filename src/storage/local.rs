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

    /// Read the data blob at the given relative path from local disk.
    ///
    /// The path should be a relative content-addressed path (e.g.,
    /// `d/dd4/dd4ce/dd4ce38e...`). The backend prepends `self.datadir`
    /// to resolve the full filesystem path.
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
    /// `d/dd4/dd4ce/dd4ce38e...`), consistent with `AmazonS3::write_data`.
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
}
