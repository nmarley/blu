use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::{fs, path::PathBuf};

use crate::hash::Hash;

use super::encrypted::Encrypted;

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Eq)]
pub struct Entry {
    pub(crate) paths: HashSet<PathBuf>,
    pub(crate) filetype: String,

    pub(crate) hash: Hash,
    pub(crate) size: usize,
    pub(crate) enc: Option<Encrypted>,

    pub(crate) tags: Vec<String>,     // TODO: proper tagging, or... ?
    pub(crate) notes: Option<String>, // free-form text
}

impl Entry {
    pub fn get_enc_ref(&self) -> &Option<Encrypted> {
        &self.enc
    }

    pub fn get_enc(&self) -> Option<Encrypted> {
        self.enc.clone()
    }

    pub fn read_filedata(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let path = self.paths.iter().next().unwrap();
        let data = fs::read(path)?;
        Ok(data)
    }

    pub fn set_encrypted(&mut self, enc: Encrypted) -> Result<(), Box<dyn std::error::Error>> {
        self.enc = Some(enc);
        Ok(())
    }

    pub fn get_hash(&self) -> Hash {
        self.hash.clone()
    }
}
