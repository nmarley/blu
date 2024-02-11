use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::age::BlackBox;
use crate::blob::{BlobIndex, BLOB_INDEX_FILENAME};
use crate::block::{PlainIndex, INDEX_FILENAME};
use crate::io::BlackBoxSerializable;
use crate::storage::{AmazonS3, Local, StorageBackend};
use crate::tag::{TagIndex, TAG_INDEX_FILENAME};

/// Backend config structures, one for each supported backend.
pub mod backend;

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

/// Config is the configuration for blu. It is stored in the .blu directory in
/// the config.(json|toml) file.
#[derive(Debug, PartialEq, Serialize, Deserialize, Eq)]
#[serde(default)]
pub struct Config {
    // TODO: idk if this is used at all ...
    blu_version: String,
    // TODO: remove this, unused (but just for now?)
    data_key_files: Vec<String>,

    // base dir
    #[serde(skip)]
    basedir: PathBuf,

    // TODO: multiple backends
    /// Storage backend for encrypted data blobs
    backend: backend::BackendConfig,

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
            backend: backend::BackendConfig::default(),
            blu_version: env!("CARGO_PKG_VERSION").to_string(),
            data_key_files: vec![],
            basedir: PathBuf::from("."),
            prune_deleted: false,
            prune_dangling: false,
            plain_index_filename: INDEX_FILENAME.into(),
            tag_index_filename: TAG_INDEX_FILENAME.into(),
            blob_index_filename: BLOB_INDEX_FILENAME.into(),
        }
    }
}

/// read_config reads the config from the .blu directory in the base_dir.
pub async fn read_config<P: AsRef<Path>>(
    base_dir: P,
) -> Result<Config, Box<dyn std::error::Error>> {
    let cfg_dir = base_dir.as_ref().join(".blu");
    let config_toml = cfg_dir.join("config.toml");
    // TODO: remove deprecated JSON configs in v0.5.x
    let config_json = cfg_dir.join("config.json");

    // Avoid toctou race condition
    // https://en.wikipedia.org/wiki/Time-of-check_to_time-of-use

    let mut cfg: Config = match tokio::fs::read_to_string(config_toml).await {
        Ok(toml_str) => match toml::from_str(&toml_str) {
            Ok(toml_cfg) => toml_cfg,
            Err(e) => return Err(e.into()),
        },
        Err(_) => {
            match tokio::fs::read_to_string(config_json).await {
                Ok(json_str) => match serde_json::from_str(&json_str) {
                    Ok(json_cfg) => {
                        println!("WARNING: using deprecated JSON config file, please update to TOML format");
                        json_cfg
                    }
                    Err(e) => return Err(e.into()),
                },
                Err(_) => return Err("Could not read either TOML or JSON config file".into()),
            }
        }
    };

    cfg.basedir = base_dir.as_ref().to_path_buf();
    Ok(cfg)
}

/// macro to write load_index, load_tag_index, load_blob_index, etc. ...
macro_rules! load_index {
    // TODO: implement as independent fn in Config, then wrap with impl version pass in path
    ($name: ident, $idx_struct_name:ident, $idx_filename_varname:ident) => {
        /// $name loads the index from the idxdir.
        pub fn $name(&self, bbox: &BlackBox) -> Option<$idx_struct_name> {
            let index_path = self.idxdir().join(&self.$idx_filename_varname);
            // info!("In config, index_path = {:?}", index_path);
            // read index file data or return None
            let index_data: Vec<u8> = fs::read(index_path).ok()?;
            // deserialize + decompress + decrypt index or return None
            $idx_struct_name::read(&index_data[..], bbox).ok()
        }
    };
}

/// macro to write write_index, write_tag_index, write_blob_index, etc. ...
macro_rules! write_index {
    ($name: ident, $idx_struct_name:ident, $idx_filename_varname:ident) => {
        /// $name writes the index to the idxdir.
        pub async fn $name(
            &self,
            idx: &$idx_struct_name,
            bbox: &BlackBox,
        ) -> Result<(), Box<dyn std::error::Error>> {
            use tokio::fs::File;
            use tokio::io::AsyncWriteExt;
            let index_path = self.idxdir().join(&self.$idx_filename_varname);
            // encrypt + compress + serialize index to buf
            let mut buf = vec![];
            idx.write(&mut buf, bbox)?;
            // write to file
            let mut file = File::create(index_path).await?;
            file.write_all(&buf).await?;
            file.flush().await?;
            Ok(())
        }
    };
}

impl Config {
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

    write_index!(write_blob_index, BlobIndex, blob_index_filename);
    write_index!(write_tag_index, TagIndex, tag_index_filename);
    write_index!(write_plain_index, PlainIndex, plain_index_filename);

    /// Initializes the storage backend based on `backend` field in config.
    pub fn init_storage_backend(
        &self,
    ) -> Result<Box<dyn StorageBackend>, Box<dyn std::error::Error>> {
        match self.backend {
            backend::BackendConfig::Local(ref local_backend) => {
                Ok(Box::new(Local::new(&local_backend.path)))
            }
            backend::BackendConfig::AmazonS3(ref s3_backend) => Ok(Box::new(AmazonS3::new(
                &s3_backend.bucket,
                s3_backend.prefix.clone(),
            ))),
            #[allow(unreachable_patterns)]
            _ => Err("Unsupported backend".into()),
        }
    }
}

#[cfg(test)]
pub(crate) mod test {
    use super::{BlackBox, Config};
    use crate::age::test::{TEST_AGE_SECRET_KEY, TEST_AGE_SECRET_KEY_PATH};

    const TEST_DIR_T0: &str = "test/old/t0/";
    const TEST_DIR_T1: &str = "test/old/t1/";
    // const TEST_DIR_T2: &str = "test/old/t2/";
    const TEST_DIR_BLOCKS_T4: &str = "test/blocks/t4/";

    #[tokio::test]
    async fn read_config() {
        assert!(super::read_config(TEST_DIR_T0).await.is_err());
        let cfg = super::read_config(TEST_DIR_T1).await.unwrap();
        // dbg!(&cfg);

        assert_eq!(
            cfg,
            Config {
                basedir: TEST_DIR_T1.into(),
                blu_version: "0.0.1".to_string(),
                data_key_files: vec![TEST_AGE_SECRET_KEY_PATH.to_string()],
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn load_plain_index() {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let cfg = super::read_config(TEST_DIR_BLOCKS_T4).await.unwrap();
        let index_opt = cfg.load_plain_index(&bbox);
        assert!(index_opt.is_some());
        let _index = index_opt.unwrap();
    }
}
