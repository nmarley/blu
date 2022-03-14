use crate::metadata::Encrypted;
use std::path::{Path, PathBuf};

#[derive(Default, Debug)]
pub struct Manager {
    datadir: PathBuf,
}

impl Manager {
    pub fn new<P: AsRef<Path> + std::fmt::Debug>(datadir: P) -> Self {
        Self {
            datadir: datadir.as_ref().to_path_buf(),
        }
    }

    pub fn delete_encrypted(&self, enc: &Encrypted) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}
