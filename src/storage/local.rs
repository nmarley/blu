use async_trait::async_trait;
use std::fs;
use std::path::{Path, PathBuf};

use crate::hash::Hash;

use super::StorageBackend;

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
}

#[async_trait]
impl StorageBackend for Local {
    fn read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let data = fs::read(path)?;
        Ok(data)
    }

    async fn async_read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let data = tokio::fs::read(path).await?;
        Ok(data)
    }

    fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let hash_path = super::path_for(hash)?;
        let path = self.datadir.join(hash_path).to_path_buf();

        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, data)?;
        Ok(path)
    }
}
