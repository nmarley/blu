use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

// TODO: implement backends -- probably a trait
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Backend {
    Local,
    S3,
}

// for now locked to just Age keys, for simplicity
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum KeyType {
    // RSA,
    // DSA,
    // ECDSA,
    // Ed25519,
    Age,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyID {
    r#type: KeyType,
    public_key: String, // TODO: Vec<u8> ?
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub backend: Backend,
    pub blu_version: String,
    pub data_keys: Vec<String>,
    pub metadata_key_id: KeyID,
}

pub fn read_config<P: AsRef<Path> + std::fmt::Debug>(
    base_dir: P,
) -> Result<Config, Box<dyn std::error::Error>> {
    // dbg!(&base_dir);

    let cfg_dir = base_dir.as_ref().join(".blu");
    // dbg!(&cfg_dir);

    // serde into a Config
    let config_filename = cfg_dir.join("config.json");
    // dbg!(&config_filename);

    // Avoid toctou race condition
    // https://en.wikipedia.org/wiki/Time-of-check_to_time-of-use
    let cfg_data = fs::read_to_string(config_filename)?;
    // dbg!(&cfg_data);
    let cfg: Config = serde_json::from_str(&cfg_data)?;
    Ok(cfg)
}

#[cfg(test)]
pub(crate) mod test {
    pub(crate) const TEST_AGE_SECRET_KEY: &str =
        "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";

    const TEST_CONFIG_DIR_T0: &str = "test/t0/";
    const TEST_CONFIG_DIR_T1: &str = "test/t1/";
    // const TEST_CONFIG_DIR_T2: &str = "test/t2/";

    use super::{Backend, Config, KeyID, KeyType};
    #[test]
    fn read_config() {
        let rando_age_key_id: KeyID = KeyID {
            r#type: KeyType::Age,
            public_key: "age12mqsq4tcdvhl3ef8a4vnq0699p40t4rr867vtga4wecn0v45gchqg9sevz"
                .to_string(),
        };

        assert!(super::read_config(TEST_CONFIG_DIR_T0).is_err());
        let cfg = super::read_config(TEST_CONFIG_DIR_T1).unwrap();
        dbg!(&cfg);

        assert_eq!(
            cfg,
            Config {
                backend: Backend::Local,
                blu_version: "0.0.1".to_string(),
                data_keys: vec![TEST_AGE_SECRET_KEY.to_string()],
                metadata_key_id: rando_age_key_id,
            }
        );
    }
}
