use std::io::Write;
use std::{env, fs};

pub mod age;
pub mod clap;
pub mod config;
pub mod magic;
pub mod metadata;

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
use crate::age::BlackBox;

// also: consider an internal webserver which serves up the UI for blu
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // TODO: handle cmd-line args w/clap
    // let mut args: Args = Args::parse();
    // if args.num_crawlers < 1 || args.num_crawlers > 999 {
    //     args.num_crawlers = 96; // how to get default here?
    // }
    // dbg!(&args);

    // let key = read-key-from-.blu/metadata.json;
    // decrypt somehow?

    let args: Vec<String> = env::args().collect();
    let dir = &args[1];

    // let cfg = config::read_config(dir);
    // dbg!(&cfg);

    // TODO: _iff_ we want to chdir before indexing, **HERE** is where
    let index = metadata::Index::new(dir)?;
    // TODO: ... and HERE is where to change back

    let mut compressed = Vec::new();
    let _ = index.write(&mut compressed)?;

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let enc_idx = bbox.encrypt(&compressed).unwrap();

    let mut file = fs::File::create("test-idx-compressed.dat")?;
    file.write_all(&enc_idx)?;

    Ok(())
}
