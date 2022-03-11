use std::io::Write;
use std::{env, fs};

pub mod age;
pub mod clap;
pub mod config;
pub mod magic;
pub mod metadata;

const TEST_AGE_SECRET_KEY: &str =
    "AGE-SECRET-KEY-13QFLW9V8FWEC7F63TQ5K2PY9E8CC8HMTXHP0VRZT45Y8KS44X4NSDGYA94";
const TEST_PASSPHRASE_ENIGMA: &str = "correct horse battery staple";
use crate::age::passphrase_encrypt;
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

    let cfg = config::read_config(dir);
    dbg!(&cfg);

    // TODO: _iff_ we wanted to chdir before indexing, **HERE** is where to do
    // it
    let index = metadata::Index::new(dir)?;
    // dbg!(&index);
    // TODO: ... and HERE is where to change back

    // let serialized_map = metadata::ser_map(&map_files)?;
    let serialized_map = index.serialize()?;
    dbg!(&serialized_map);
    // println!("{}", &hex::encode(&serialized_map));

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let enc_map = bbox.encrypt(&serialized_map).unwrap();
    // dbg!(&enc_map);
    println!("{}", &hex::encode(&enc_map));

    let mut file = fs::File::create("test-idx.dat")?;
    file.write_all(&enc_map)?;

    let val = passphrase_encrypt(TEST_AGE_SECRET_KEY.as_bytes(), TEST_PASSPHRASE_ENIGMA)?;
    println!("{}", &hex::encode(&val));

    Ok(())
}
