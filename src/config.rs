use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::age::BlackBox;
use crate::blob::{BlobIndex, BLOB_INDEX_FILENAME};
use crate::block::{PlainIndex, INDEX_FILENAME};
use crate::io::BlackBoxSerializable;
use crate::metadata::{Index, INDEX_FILENAME as V1_INDEX_FILENAME};
use crate::tagger::{TagIndex, TAG_INDEX_FILENAME};

// TODO: implement backends -- probably a trait
#[derive(Debug, PartialEq, Serialize, Deserialize, Eq)]
pub enum Backend {
    Local,
    S3,
}

impl Default for Backend {
    fn default() -> Backend {
        Backend::Local
    }
}

// for now locked to just Age keys, for simplicity
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Hash, Eq, PartialOrd, Ord)]
pub enum KeyType {
    // RSA,
    // DSA,
    // ECDSA,
    // Ed25519,
    Age,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Hash, Eq, PartialOrd, Ord)]
pub struct KeyID {
    r#type: KeyType,
    public_key: String, // TODO: Vec<u8> ?
}

const DEFAULT_DATADIR: &str = ".blu/data";
// TODO: also, don't worry for now, this is not MVP. For now we can hard-code
// paths in data_key_files instead.
//
// TODO: how to do this w/const? possible or nah?
// const DEFAULT_DATA_KEY_FILES: &'static str = "$HOME/.blu/secrets/blu.key";
//                                              "$HOME/.blu/secrets/blu.pub";
// std::env::get("HOME")

#[derive(Debug, PartialEq, Serialize, Deserialize, Eq)]
#[serde(default)]
pub struct Config {
    pub backend: Backend,
    pub blu_version: String,
    pub data_key_files: Vec<String>,

    // base dir
    #[serde(skip)]
    pub basedir: PathBuf,

    // The datadir should hold encrypted data and metadata. Priv keys should
    // never be stored here, even encrypted.
    pub datadir: Option<PathBuf>,

    // should blu delete Encrypted from filesystem, if the plain version was deleted?
    pub prune_deleted: bool,
    // should blu delete dangling Encrypted from filesystem?
    pub prune_dangling: bool,

    pub plain_index_filename: PathBuf,
    pub tag_index_filename: PathBuf,
    pub blob_index_filename: PathBuf,
    // deprecated
    pub v1_plain_index_filename: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: Backend::default(),
            blu_version: env!("CARGO_PKG_VERSION").to_string(),
            data_key_files: vec![],
            basedir: PathBuf::from("."),
            datadir: Some(DEFAULT_DATADIR.into()),
            prune_deleted: false,
            prune_dangling: false,
            plain_index_filename: INDEX_FILENAME.into(),
            tag_index_filename: TAG_INDEX_FILENAME.into(),
            blob_index_filename: BLOB_INDEX_FILENAME.into(),
            // deprecated
            v1_plain_index_filename: V1_INDEX_FILENAME.into(),
        }
    }
}

pub fn read_config<P: AsRef<Path>>(base_dir: P) -> Result<Config, Box<dyn std::error::Error>> {
    let cfg_dir = base_dir.as_ref().join(".blu");
    let config_filename = cfg_dir.join("config.json");

    // Avoid toctou race condition
    // https://en.wikipedia.org/wiki/Time-of-check_to_time-of-use
    let cfg_data = fs::read_to_string(config_filename)?;

    let mut cfg: Config = serde_json::from_str(&cfg_data)?;
    cfg.basedir = base_dir.as_ref().to_path_buf();
    Ok(cfg)
}

/// macro to write load_index, load_tag_index, load_blob_index, etc. ...
macro_rules! load_index {
    ($name: ident, $idx_struct_name:ident, $idx_filename_varname:ident) => {
        pub fn $name(&self, bbox: &BlackBox) -> Option<$idx_struct_name> {
            let index_path = self.datadir().join(&self.$idx_filename_varname);
            // read index file data or return None
            let index_data: Vec<u8> = match fs::read(index_path) {
                Ok(data) => data,
                Err(_) => return None,
            };
            // deserialize + decompress + decrypt index or return None
            match $idx_struct_name::read(&index_data[..], bbox) {
                Ok(index) => Some(index),
                Err(_e) => None,
            }
        }
    };
}

impl Config {
    pub fn datadir(&self) -> PathBuf {
        let rel_dir = match self.datadir.clone() {
            Some(s) => s,
            None => PathBuf::from(DEFAULT_DATADIR),
        };
        self.basedir.join(rel_dir)
    }

    load_index!(load_blob_index, BlobIndex, blob_index_filename);
    load_index!(load_tag_index, TagIndex, tag_index_filename);
    load_index!(load_plain_index, PlainIndex, plain_index_filename);
    // deprecated
    load_index!(v1_load_index, Index, v1_plain_index_filename);

    pub fn write_blob_index(
        &self,
        blob_index: &BlobIndex,
        bbox: &BlackBox,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let index_path = self.datadir().join(&self.blob_index_filename);

        // encrypt + compress + serialize index to buf
        let mut buf = vec![];
        blob_index.write(&mut buf, bbox)?;
        // write to file
        std::fs::write(index_path, buf)?;

        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test {
    use super::{Backend, BlackBox, Config};
    use crate::age::test::{TEST_AGE_SECRET_KEY, TEST_AGE_SECRET_KEY_PATH};

    const TEST_DIR_T0: &str = "test/old/t0/";
    const TEST_DIR_T1: &str = "test/old/t1/";
    const TEST_DIR_T2: &str = "test/old/t2/";
    const TEST_DIR_BLOCKS_T4: &str = "test/blocks/t4/";

    #[test]
    fn read_config() {
        assert!(super::read_config(TEST_DIR_T0).is_err());
        let cfg = super::read_config(TEST_DIR_T1).unwrap();
        // dbg!(&cfg);

        assert_eq!(
            cfg,
            Config {
                backend: Backend::Local,
                basedir: TEST_DIR_T1.into(),
                blu_version: "0.0.1".to_string(),
                data_key_files: vec![TEST_AGE_SECRET_KEY_PATH.to_string()],
                ..Default::default()
            }
        );
    }

    #[test]
    fn v1_load_index() {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let cfg = super::read_config(TEST_DIR_T2).unwrap();
        let index_opt = cfg.v1_load_index(&bbox);

        assert!(index_opt.is_some());
        let _index = index_opt.unwrap();
    }

    #[test]
    fn load_plain_index() {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let cfg = super::read_config(TEST_DIR_BLOCKS_T4).unwrap();
        let index_opt = cfg.load_plain_index(&bbox);
        assert!(index_opt.is_some());
        let _index = index_opt.unwrap();
    }
}
