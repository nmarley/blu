use multihash::{Code, MultihashDigest};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{fmt, fs, path::Path};
use walkdir::WalkDir;

pub mod age;
pub mod clap;
pub mod magic;

use magic::Wizard;

// also: consider an internal webserver which serves up the UI for blu
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // TODO: handle cmd-line args w/clap
    // let mut args: Args = Args::parse();
    // if args.num_crawlers < 1 || args.num_crawlers > 999 {
    //     args.num_crawlers = 96; // how to get default here?
    // }
    // println!("args: {:?}", args);

    // let key = read-key-from-.blu/metadata.json;
    // decrypt somehow?

    // let conn = db.connection();
    let dir = "."; // TODO: use Getcwd() instead?
    let cfg = read_config(dir);
    println!("cfg = {:?}", cfg);

    let _map_files = index(dir)?;
    // println!("map_files = {:?}", map_files);

    // sync()
    Ok(())
}

// TODO: rename this struct ...
// FileMeta? Archive?
#[derive(PartialEq)]
pub struct Entry {
    // paths: Vec<std::path::Path>,
    paths: Vec<String>,
    filetype: String,

    hash: Vec<u8>,
    size: u64,
    enc: Option<Encrypted>,

    tags: Vec<String>,     // TODO: proper tagging, or... ?
    notes: Option<String>, // free-form text
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("paths", &self.paths)
            .field("filetype", &self.filetype)
            .field("hash", &hex::encode(&self.hash))
            .field("size", &self.size)
            .field("enc", &self.enc)
            .field("tags", &self.tags)
            .field("notes", &self.notes)
            .finish()
    }
}

#[derive(PartialEq)]
pub struct Encrypted {
    hash: Vec<u8>,
    size: u64,
    keys: Vec<KeyID>,
}

impl fmt::Debug for Encrypted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encrypted")
            .field("hash", &hex::encode(&self.hash))
            .field("size", &self.size)
            .field("keys", &self.keys)
            .finish()
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
// rsa, dsa, ecdsa and ed25519
// for now locked to just Age keys, for simplicity
pub enum KeyType {
    // RSA,
    // DSA,
    // ECDSA,
    // Ed25519,
    Age,
}

// ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIP0Z61hOKGh3YXwySlaelOr7VYrMbb8pkPzq9AXXaGIM nmarley@zeal
//
// rando age key
// # public key: age12mqsq4tcdvhl3ef8a4vnq0699p40t4rr867vtga4wecn0v45gchqg9sevz
// AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyID {
    r#type: KeyType,
    public_key: String, // TODO: Vec<u8>
}

// walk the dir and hash all regular files
// ignore block/char specials
//
// TODO: accept an SQLite3 connection for metadata writes?
fn index<P: AsRef<Path>>(
    base_dir: P,
) -> Result<HashMap<Vec<u8>, Entry>, Box<dyn std::error::Error>> {
    let mut count = 0usize;

    // TODO: only build a new hashmap if we don't get metadata from the DB already
    let mut map_files = HashMap::new();

    // chdir into base before walking
    //
    // otherwise we get paths like "./test/file.txt" if we set the base dir to
    // "./test"

    // let current_dir = env::current_dir()?;
    // env::set_current_dir(&base_dir)?;

    let wiz = Wizard::new();

    // for entry in WalkDir::new(".").into_iter().filter_map(|e| e.ok()) {
    for entry in WalkDir::new(base_dir).into_iter().filter_map(|e| e.ok()) {
        // for initial debugging
        if count == 5 {
            break;
        }

        // TODO: allow symlinks?
        if !entry.file_type().is_file() {
            continue;
        }
        count += 1;

        let metadata = fs::metadata(entry.path())?;
        let size = metadata.len();
        println!("{:?}: {:?} bytes", entry.path(), size);

        // TODO: streaming reads here? as some files could be GB in size...
        let filedata = fs::read(entry.path()).unwrap();
        let filetype = wiz
            .get_filetype(&filedata, size)
            .unwrap_or_else(|_| "other".into());
        // dbg!(&filetype);
        let mh = Code::Sha2_512.digest(&filedata);

        // e2 is a reference to the entry in the hashmap ...
        let e2 = map_files.entry(mh.to_bytes()).or_insert(Entry {
            filetype,
            paths: vec![],
            size,
            hash: mh.to_bytes(),
            enc: None,
            tags: vec![],
            notes: None,
        });
        // ... so when it gets modified here, it is updated in the hashmap
        // TODO: fix this, serialize correctly
        e2.paths.push(entry.path().display().to_string());

        // println!("e2 = {:?}", e2);
        // println!("========================================================================");
    }

    // now go back to previous state
    // env::set_current_dir(current_dir)?;

    Ok(map_files)
}

#[cfg(test)]
mod test {
    const BASE_DIR: &str = "./test";

    #[test]
    fn index() {
        let map_files = super::index(BASE_DIR).unwrap();
        // dbg!(&map_files);
        let art1_hash = hex::decode("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6").unwrap();
        let entry = map_files.get(&art1_hash).unwrap();

        assert_eq!(
            super::Entry {
                paths: vec![
                    "./test/art1_dup_en.txt".to_string(),
                    "./test/article1_en.txt".to_string()
                ],
                filetype: "ASCII text".to_string(),
                size: 171,
                hash: art1_hash,
                enc: None,
                tags: vec![],
                notes: None,
            },
            *entry
        );
    }

    const TEST_AGE_SECRET_KEY: &str =
        "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
    #[test]
    fn encrypt_decrypt() {
        let bbox = crate::age::BlackBox::new(&vec![TEST_AGE_SECRET_KEY]);
        let data: [u8; 5] = [0x64, 0xff, 0xcd, 0xbf, 0xbb];

        let encrypted = bbox.encrypt(&data).unwrap();
        // dbg!(&encrypted);

        let decrypted = bbox.decrypt(&encrypted).unwrap();
        // dbg!(&decrypted);
        assert_eq!(decrypted, &data[..]);
    }

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

// TODO: serde fields ...
// TODO: implement backends -- probably a trait
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Backend {
    Local,
    S3,
}

// TODO: serde fields ...
// TODO: multiple backends?

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub backend: Backend,
    pub blu_version: String,
    pub data_keys: Vec<String>,
    pub metadata_key_id: KeyID,
}

fn read_config<P: AsRef<Path> + std::fmt::Debug>(
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
