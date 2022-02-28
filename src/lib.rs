#![allow(dead_code)] // remove this later

use clap::Parser;
use multihash::{Code, MultihashDigest};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::str;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[clap(version = "0.1")]
struct Args {
    /// Number of crawlers to run in parallel
    #[clap(short = 't', long, default_value_t = 96)]
    pub num_crawlers: u32,

    /// Number of DNS server threads
    #[clap(short, long, default_value = "4")]
    pub dns_threads: u32,

    /// UDP port to listen on
    #[clap(short, long, default_value = "53")]
    pub port: u16,

    /// Number of seconds to sleep before printing stats
    #[clap(short, long, default_value = "1")]
    pub stats_sleep_seconds: u16,

    /// Wipe list of banned nodes
    #[clap(long)]
    pub wipeban: bool,

    /// Tor proxy IP/Port
    #[clap(short = 'o', long = "onion", value_name = "ip:port")]
    pub tor: Option<String>,

    /// Flag filter (combine network filters with bitwise &)
    #[clap(short = 'w', long)]
    pub filter: Option<u32>,
}

// also: consider an internal webserver which serves up the UI for blu
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Args = Args::parse();

    if args.num_crawlers < 1 || args.num_crawlers > 999 {
        args.num_crawlers = 96; // how to get default here?
    }
    println!("args: {:?}", args);

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
#[derive(Debug, PartialEq)]
pub struct Entry {
    // paths: Vec<std::path::Path>,
    paths: Vec<String>,
    filetype: String, // TODO: enum or elsething -- need a full list of file magic or at least major ones

    // TODO: probably re-think this organization ...
    unlocked: SizeHash,
    locked: Option<SizeHash>,

    tags: Vec<String>,     // TODO: proper tagging, or... ?
    notes: Option<String>, // free-form text
}

#[derive(Debug, PartialEq)]
pub struct SizeHash {
    size: u64,
    hash: Vec<u8>,
    keys: Option<Vec<KeyID>>,
}

#[derive(Debug, PartialEq)]
pub enum KeyType {
    Ed25519,
    Age,
}

// ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIP0Z61hOKGh3YXwySlaelOr7VYrMbb8pkPzq9AXXaGIM nmarley@zeal
//
// rando age key
// # public key: age12mqsq4tcdvhl3ef8a4vnq0699p40t4rr867vtga4wecn0v45gchqg9sevz
// AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94
#[derive(Debug, PartialEq)]
pub struct KeyID {
    r#type: KeyType,
    public_key: String, // TODO: Vec<u8>
}

// pub trait Locked {
//     fn keys() -> Vec<KeyID>,
// }

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
    env::set_current_dir(&base_dir).unwrap();

    for entry in WalkDir::new(".").into_iter().filter_map(|e| e.ok()) {
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
        let mh = Code::Sha2_512.digest(&fs::read(entry.path()).unwrap());

        // e2 is a reference to the entry in the hashmap ...
        let e2 = map_files.entry(mh.to_bytes()).or_insert(Entry {
            paths: vec![],
            filetype: "PDF".to_string(), // TODO: Get file magic
            unlocked: SizeHash {
                size,
                hash: mh.to_bytes(),
                keys: None,
            },
            locked: None,
            tags: vec![],
            notes: None,
        });
        // ... so when it gets modified here, it is updated in the hashmap
        // TODO: fix this, serialize correctly
        e2.paths.push(entry.path().display().to_string());

        println!("e2 = {:?}", e2);
        println!("========================================================================");
    }

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
                    "./art1_dup_en.txt".to_string(),
                    "./article1_en.txt".to_string()
                ],
                filetype: "PDF".to_string(),
                unlocked: super::SizeHash {
                    size: 171,
                    hash: art1_hash,
                    keys: None,
                },
                locked: None,
                tags: vec![],
                notes: None,
            },
            *entry
        );
    }
}

// pub struct Backend { }
// TODO: serde fields ...
// TODO: implement backends -- probably a trait
#[derive(Debug)]
pub enum Backend {
    Local,
    S3,
}

// TODO: serde fields ...
// TODO: multiple backends?
#[derive(Debug)]
pub struct Config {
    pub metadata_key_id: KeyID,
    pub backend: Backend,
}

fn read_config<P: AsRef<Path>>(base_dir: P) -> Result<Config, Box<dyn std::error::Error>> {
    // TODO: MOVE TO TEST
    let rando_age_key_id: KeyID = KeyID {
        r#type: KeyType::Age,
        public_key: "age12mqsq4tcdvhl3ef8a4vnq0699p40t4rr867vtga4wecn0v45gchqg9sevz".to_string(),
    };

    let cfg_dir = base_dir.as_ref().join(".blu");
    // println!("cfg_dir = {:?}", cfg_dir);

    // serde into a Config
    let config_file = cfg_dir.join("config.json");
    println!("config_file = {:?}", config_file);

    // read_file + serde or '?' at the end for errors ... good
    // https://stackoverflow.com/a/32384768
    //
    // Note that many times you want to do something with the file, like read
    // it. In those cases, it makes more sense to just try to open it and deal
    // with the Result. This eliminates a race condition between "check to see
    // if file exists" and "open file if it exists". If all you really care
    // about is if it exists...
    // https://en.wikipedia.org/wiki/Time-of-check_to_time-of-use

    Ok(Config {
        metadata_key_id: rando_age_key_id,
        backend: Backend::Local,
    })
}

// fn sync() -> Result<(), Box<dyn std::error::Error>> {
//     Err("something didn't work".into())
//     // Ok(())
// }
