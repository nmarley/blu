//! Local filesystem storage backend implementation.

use std::path::{Path, PathBuf};

use crate::error::BluError;
use crate::hash::Hash;

/// Local storage backend for managing data on a local filesystem.
#[derive(Default, Debug, Clone)]
pub struct Local {
    datadir: PathBuf,
}

impl Local {
    /// Create a new Local storage backend with the given datadir.
    pub fn new<P: AsRef<Path>>(datadir: P) -> Self {
        Self {
            datadir: datadir.as_ref().to_path_buf(),
        }
    }

    /// Read the data blob at the given relative path from local disk.
    ///
    /// The path should be a relative content-addressed path (e.g.,
    /// `d/dd4/dd4ce/dd4ce38e...`). The backend prepends `self.datadir`
    /// to resolve the full filesystem path.
    pub async fn read_data(&self, path: &Path) -> Result<Vec<u8>, BluError> {
        let full_path = self.datadir.join(path);
        let data = tokio::fs::read(&full_path).await?;
        Ok(data)
    }

    /// Write data to a content-addressed path derived from the hash.
    ///
    /// Returns the relative content-addressed path (e.g.,
    /// `d/dd4/dd4ce/dd4ce38e...`), consistent with `AmazonS3::write_data`.
    pub async fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, BluError> {
        let hash_path = super::path_for(hash)?;
        let full_path = self.datadir.join(&hash_path);

        tokio::fs::create_dir_all(full_path.parent().unwrap()).await?;
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
