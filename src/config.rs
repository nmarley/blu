use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::age::BlackBox;
use crate::blob::{BlobIndex, BLOB_INDEX_FILENAME};
use crate::block::{PlainIndex, INDEX_FILENAME};
use crate::io::BlackBoxSerializable;
use crate::tagger::{TagIndex, TAG_INDEX_FILENAME};

// TODO: implement backends -- probably a trait
/// Backend is the storage backend for blu. Currently only local filesystem is
/// supported.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize, Eq)]
pub enum Backend {
    /// Local filesystem
    #[default]
    Local,
    /// Amazon S3
    S3,
}

// for now locked to just Age keys, for simplicity
/// KeyType is the type of key used to encrypt/decrypt data. Currently only Age
/// keys are supported.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Hash, Eq, PartialOrd, Ord)]
pub enum KeyType {
    // RSA,
    // DSA,
    // ECDSA,
    // Ed25519,
    /// Age key
    Age,
}

/// KeyID is a unique identifier for a key. It is a combination of the key type
/// and public key, but in reality is just the public key.
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

/// Config is the configuration for blu. It is stored in the .blu directory in
/// the config.json file.
#[derive(Debug, PartialEq, Serialize, Deserialize, Eq)]
#[serde(default)]
pub struct Config {
    backend: Backend,
    blu_version: String,
    data_key_files: Vec<String>,

    // base dir
    #[serde(skip)]
    basedir: PathBuf,

    // The datadir should hold encrypted data and metadata. Priv keys should
    // never be stored here, even encrypted.
    datadir: Option<PathBuf>,

    // should blu delete Encrypted from filesystem, if the plain version was deleted?
    prune_deleted: bool,
    // should blu delete dangling Encrypted from filesystem?
    prune_dangling: bool,

    plain_index_filename: PathBuf,
    tag_index_filename: PathBuf,
    blob_index_filename: PathBuf,
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
        }
    }
}

/// read_config reads the config from the .blu directory in the base_dir.
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
        /// $name loads the index from the idxdir.
        pub fn $name(&self, bbox: &BlackBox) -> Option<$idx_struct_name> {
            let index_path = self.idxdir().join(&self.$idx_filename_varname);
            info!("In config, index_path = {:?}", index_path);
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
    /// Returns the datadir WITHIN the base directory for blu. This is the
    /// directory that holds the encrypted data blobs.
    ///
    /// Probably not a great design, and I'm open to changing this in the
    /// future.
    pub fn datadir(&self) -> PathBuf {
        let rel_dir = match self.datadir {
            Some(ref s) => s.as_path(),
            // TODO: use bludir() + join
            None => Path::new(DEFAULT_DATADIR),
        };
        self.basedir.join(rel_dir)
    }

    /// Returns the .blu dir within the base directory. This holds the config,
    /// and nested indexes and data dirs.
    pub fn bludir(&self) -> PathBuf {
        self.basedir.join(".blu")
    }

    /// Returns the directory used to hold the indexes.
    pub fn idxdir(&self) -> PathBuf {
        self.bludir().join("indexes")
    }

    load_index!(load_blob_index, BlobIndex, blob_index_filename);
    load_index!(load_tag_index, TagIndex, tag_index_filename);
    load_index!(load_plain_index, PlainIndex, plain_index_filename);

    /// write_blob_index writes the blob index to the idxdir.
    pub fn write_blob_index(
        &self,
        blob_index: &BlobIndex,
        bbox: &BlackBox,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let index_path = self.idxdir().join(&self.blob_index_filename);

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
    // const TEST_DIR_T2: &str = "test/old/t2/";
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
    fn load_plain_index() {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let cfg = super::read_config(TEST_DIR_BLOCKS_T4).unwrap();
        let index_opt = cfg.load_plain_index(&bbox);
        assert!(index_opt.is_some());
        let _index = index_opt.unwrap();
    }
}
