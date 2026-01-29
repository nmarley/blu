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

impl StorageBackend for Local {
    fn read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let data = std::fs::read(path)?;
        Ok(data)
    }

    fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let hash_path = super::path_for(hash)?;
        let path = self.datadir.join(hash_path).to_path_buf();

        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, data)?;
        Ok(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(path.exists())
    }

    fn delete(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        fs::remove_file(path)?;
        Ok(())
    }
}
