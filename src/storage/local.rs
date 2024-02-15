use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::fs::{self, File};
use tokio::io::AsyncRead;

use crate::hash::Hash;

use super::StorageBackend;
use super::StorageError;

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
    async fn read_data(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Unpin + Send>, StorageError> {
        let file = File::open(path).await.map_err(StorageError::IoError)?;
        Ok(Box::new(file) as Box<dyn AsyncRead + Unpin + Send>)
    }

    async fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, StorageError> {
        let hash_path = match super::path_for(hash) {
            Ok(path) => path,
            Err(err) => return Err(StorageError::HashError(err)),
        };
        let path = self.datadir.join(hash_path).to_path_buf();

        fs::create_dir_all(path.parent().unwrap()).await?;
        fs::write(&path, data).await?;
        Ok(path)
    }
}
