use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::age::BlackBox;
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

#[derive(Default, Debug, PartialEq, Serialize, Deserialize, Eq)]
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

impl Config {
    pub fn load_index(
        &self,
        bbox: &BlackBox,
    ) -> Result<Option<PlainIndex>, Box<dyn std::error::Error>> {
        self.v2_load_index(bbox)
    }

    pub fn v2_load_index(
        &self,
        bbox: &BlackBox,
    ) -> Result<Option<PlainIndex>, Box<dyn std::error::Error>> {
        let index_path = self.datadir().join(INDEX_FILENAME);

        // todo: filter index.dat
        // if error loading this (e.g. file doesn't exist) then return None or
        // build a new index ... consider building a new one instead of None.
        let index_data: Vec<u8> = match fs::read(index_path) {
            Ok(data) => data,
            Err(_) => return Ok(None),
        };
        // read index
        let index = PlainIndex::read(&index_data[..], bbox)?;
        Ok(Some(index))
    }

    pub fn v1_load_index(
        &self,
        bbox: &BlackBox,
    ) -> Result<Option<Index>, Box<dyn std::error::Error>> {
        // should always sit in same directory with the data
        // this should _not_ be user-configurable (e.g. should not be in Config)
        let index_path = self.datadir().join(V1_INDEX_FILENAME);
        // todo: filter index.dat

        // if error loading this (e.g. file doesn't exist) then return None or
        // build a new index ... consider building a new one instead of None.
        let index_data: Vec<u8> = match fs::read(index_path) {
            Ok(data) => data,
            Err(_) => return Ok(None),
        };

        // read index
        let index = Index::read(&index_data[..], bbox)?;

        Ok(Some(index))
    }

    pub fn datadir(&self) -> PathBuf {
        let rel_dir = match self.datadir.clone() {
            Some(s) => s,
            None => PathBuf::from(DEFAULT_DATADIR),
        };
        self.basedir.join(rel_dir)
    }

    pub fn load_tag_index(
        &self,
        bbox: &BlackBox,
    ) -> Result<Option<TagIndex>, Box<dyn std::error::Error>> {
        let index_path = self.datadir().join(TAG_INDEX_FILENAME);

        // todo: filter index.dat
        // if error loading this (e.g. file doesn't exist) then return None or
        // build a new index ... consider building a new one instead of None.
        let index_data: Vec<u8> = match fs::read(index_path) {
            Ok(data) => data,
            Err(_) => return Ok(None),
        };
        // read index
        let index = TagIndex::read(&index_data[..], bbox)?;
        Ok(Some(index))
    }
}

#[cfg(test)]
pub(crate) mod test {
    use super::{Backend, BlackBox, Config};
    use crate::age::test::{TEST_AGE_SECRET_KEY, TEST_AGE_SECRET_KEY_PATH};

    const TEST_DIR_T0: &str = "test/t0/";
    const TEST_DIR_T1: &str = "test/t1/";
    const TEST_DIR_T2: &str = "test/t2/";
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
        let index = cfg.v1_load_index(&bbox).unwrap();

        assert!(index.is_some());
        let _index = index.unwrap();
    }

    #[test]
    fn v2_load_index() {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let cfg = super::read_config(TEST_DIR_BLOCKS_T4).unwrap();
        let index = cfg.v2_load_index(&bbox).unwrap();

        assert!(index.is_some());
        let _index = index.unwrap();
    }

    #[test]
    fn load_index() {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let cfg = super::read_config(TEST_DIR_BLOCKS_T4).unwrap();
        let index = cfg.load_index(&bbox).unwrap();
        dbg!(&index);

        assert!(index.is_some());
        let _index = index.unwrap();
    }
}
