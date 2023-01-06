use std::env;

use blu::age::BlackBox;
use blu::config;
use blu::io::BlackBoxSerializable;
use blu::tagger::TagIndex;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <dir-to-index>", args.next().unwrap());
        std::process::exit(1);
    }

    let dir = &args.nth(1).unwrap();
    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;
    // dbg!(&cfg);

    let index = cfg
        .load_index(&bbox)
        .map_err(|e| {
            eprintln!("Unable to load index. Please create index via `index` subcommand");
            eprintln!("More info: {}", e);
            e
        })?
        .unwrap();
    dbg!(&index);

    // let mut tag_index = cfg
    //     .load_tag_index(&bbox)
    //     .map_err(|e| {
    //         eprintln!("Unable to load tag index. Please create tag index via `tag` subcommand");
    //         eprintln!("More info: {}", e);
    //         e
    //     })?
    //     .unwrap();

    let mut tag_index = TagIndex::new();

    let mut hashes = index.iter_hashes();
    // let my_hash = hashes.iter().next().unwrap();
    let my_hash = hashes.next().unwrap();
    tag_index.add_tag(my_hash, "test");
    tag_index.add_tag(my_hash, "passport");
    tag_index.add_tag(my_hash, "brazil");

    for t in tag_index.get_tags(my_hash).iter() {
        dbg!(&t);
    }
    tag_index.remove_tag(my_hash, "test");

    for t in tag_index.get_tags(my_hash).iter() {
        dbg!(&t);
    }

    // this is more of an 'encrypt' method than a write
    let mut enc_idx_bytes = Vec::new();
    tag_index.write(&mut enc_idx_bytes, &bbox)?;

    // this is the actual filesystem write
    // let mut file = fs::File::create(outfile)?;
    // file.write_all(&enc_idx_bytes)?;

    Ok(())
}
