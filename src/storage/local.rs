//! Local filesystem storage backend implementation.

use std::path::{Path, PathBuf};

use crate::hash::Hash;

/// Local storage backend for managing data on a local filesystem.
#[derive(Default, Debug)]
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

    /// Read the data blob at the given path from local disk.
    pub async fn read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let data = tokio::fs::read(path).await?;
        Ok(data)
    }

    /// Write data to a content-addressed path derived from the hash.
    pub async fn write_data(
        &self,
        hash: &Hash,
        data: &[u8],
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let hash_path = super::path_for(hash)?;
        let path = self.datadir.join(hash_path).to_path_buf();

        tokio::fs::create_dir_all(path.parent().unwrap()).await?;
        tokio::fs::write(&path, data).await?;
        Ok(path)
    }

    /// Check if a blob exists at the given path.
    pub async fn exists(&self, path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(tokio::fs::try_exists(path).await?)
    }

    /// Delete a blob at the given path.
    pub async fn delete(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        tokio::fs::remove_file(path).await?;
        Ok(())
    }

    /// Write data to a known path in the backend (not hash-derived).
    pub async fn write_to_path(
        &self,
        path: &Path,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let full_path = self.datadir.join(path);
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&full_path, data).await?;
        Ok(())
    }

    /// Read data from a known path in the backend (not hash-derived).
    pub async fn read_from_path(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let full_path = self.datadir.join(path);
        let data = tokio::fs::read(&full_path).await?;
        Ok(data)
    }
}
