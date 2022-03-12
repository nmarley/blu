use crate::age::BlackBox;
use crate::metadata::Index;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

// TODO: implement backends -- probably a trait
#[derive(Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub enum KeyType {
    // RSA,
    // DSA,
    // ECDSA,
    // Ed25519,
    Age,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct KeyID {
    r#type: KeyType,
    public_key: String, // TODO: Vec<u8> ?
}

// ???
const DEFAULT_DATADIR: &str = ".blu/data";
// TODO: also, don't worry for now, this is not MVP. For now we can hard-code
// paths in data_key_files instead.
//
// // TODO: how to do this w/const? possible or nah?
// const DEFAULT_DATA_KEY_FILES: &'static str = "$HOME/.blu/secrets/blu.key";
// //                                              "$HOME/.blu/secrets/blu.pub";
// // std::env::get("HOME")

#[derive(Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub backend: Backend,
    pub blu_version: String,

    // TODO: Should be pointer to files somewhere else? Yeah ...
    // pub data_keys: Vec<String>,
    pub data_key_files: Vec<String>,

    // The purpose of the `metadata_key_id` field is just to show in the config,
    // which key has encrypted the metadata. Informational purposes only.
    // pub metadata_key_id: Option<KeyID>,

    // The datadir should hold encrypted data and metadata.
    // priv keys should never be stored here, even encrypted
    pub datadir: Option<String>,
}

pub fn read_config<P: AsRef<Path> + std::fmt::Debug>(
    base_dir: P,
) -> Result<Config, Box<dyn std::error::Error>> {
    let cfg_dir = base_dir.as_ref().join(".blu");
    let config_filename = cfg_dir.join("config.json");

    // Avoid toctou race condition
    // https://en.wikipedia.org/wiki/Time-of-check_to_time-of-use
    let cfg_data = fs::read_to_string(config_filename)?;

    let cfg: Config = serde_json::from_str(&cfg_data)?;
    Ok(cfg)
}

impl Config {
    pub fn load_index<P: AsRef<Path> + std::fmt::Debug>(
        &self,
        base_dir: P,
        bbox: &BlackBox,
    ) -> Result<Option<Index>, Box<dyn std::error::Error>> {
        // should always sit in same directory with the data
        // this should _not_ be user-configurable (e.g. should not be in Config)
        let index_path = base_dir.as_ref().join(&self.datadir()).join("index.dat");

        // if error loading this (e.g. file doesn't exist) then return None or
        // build a new index ... consider building a new one instead of None.
        let index_data: Vec<u8>;
        match fs::read(index_path) {
            Ok(data) => {
                index_data = data;
            }
            Err(_) => return Ok(None),
        };

        // decrypt idx, result is still serialized + compressed
        let idx_ser = bbox.decrypt(&index_data).unwrap();

        // read index
        let index = Index::read(&idx_ser[..])?;
        // println!

        Ok(Some(index))
    }

    pub fn datadir(&self) -> String {
        match &self.datadir {
            Some(s) => s,
            None => DEFAULT_DATADIR,
        }
        .to_string()
    }
}

#[cfg(test)]
pub(crate) mod test {
    use super::{Backend, BlackBox, Config};
    use crate::age::test::{TEST_AGE_SECRET_KEY, TEST_AGE_SECRET_KEY_PATH};

    const TEST_CONFIG_DIR_T0: &str = "test/t0/";
    const TEST_CONFIG_DIR_T1: &str = "test/t1/";
    const TEST_CONFIG_DIR_T2: &str = "test/t2/";

    #[test]
    fn read_config() {
        assert!(super::read_config(TEST_CONFIG_DIR_T0).is_err());
        let cfg = super::read_config(TEST_CONFIG_DIR_T1).unwrap();
        dbg!(&cfg);

        assert_eq!(
            cfg,
            Config {
                backend: Backend::Local,
                blu_version: "0.0.1".to_string(),
                data_key_files: vec![TEST_AGE_SECRET_KEY_PATH.to_string()],
                ..Default::default()
            }
        );
    }

    #[test]
    fn load_index() {
        let bbox = BlackBox::new(&vec![TEST_AGE_SECRET_KEY]);
        let cfg = super::read_config(TEST_CONFIG_DIR_T2).unwrap();
        let index = cfg.load_index(TEST_CONFIG_DIR_T2, &bbox).unwrap();

        assert!(index.is_some());
        let index = index.unwrap();
        dbg!(&index);
        // TODO: test index? or is loading and is_some() enough?
    }
}
