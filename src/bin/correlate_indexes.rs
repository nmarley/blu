#![allow(clippy::uninlined_format_args)]

#[macro_use]
extern crate log;

use itertools::Itertools;
use simplelog::*;
use std::env;
use std::path::Path;

use blu::age::BlackBox;
use blu::config;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    CombinedLogger::init(vec![TermLogger::new(
        LevelFilter::Info,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )])
    .unwrap();

    let mut args = env::args();
    if args.len() == 1 {
        eprintln!("usage: {} <dir-to-index>", args.next().unwrap());
        std::process::exit(1);
    }

    info!("Started correlated_indexes util");

    // move into the basedir for all operations, like `git -C <dir>`
    let basedir = &args.nth(1).unwrap();
    env::set_current_dir(basedir)?;
    let dir = Path::new(".");

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);

    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;

    let _datadir = cfg.datadir();
    info!("datadir = {:?}", _datadir);
    let plain_index = cfg.load_plain_index(&bbox).unwrap();

    let blob_index = cfg.load_blob_index(&bbox).unwrap_or_default();
    info!(
        "Blob index has {} blob files",
        blob_index.count_blob_files()
    );

    // Start with files, random blocks will make the data storage less organized and more scattered
    // on disk. Async threads won't even help with bad design.

    // let (mut files, mut blocks) = plain_index.destruct();
    info!("iterating plain_index now");

    let files_map = plain_index.files_map_ref();
    let file_hashes = files_map.keys().clone().sorted_unstable();

    for file_hash in file_hashes {
        let file_ref = files_map.get(file_hash).unwrap();
        info!("file_hash: {:?}", &file_hash.dbg_short(7));
        info!("chunks: {}", file_ref.chunkmetas.len());
        for (count, cm) in file_ref.chunkmetas.iter().enumerate() {
            info!("\t chunkmeta[{}]: {:?}", count, cm.hash.dbg_short(7));
            if !blob_index.has_chunk(&cm.hash) {
                info!("chunk hash NOT found in blob index, moving on ...");
                continue;
            }
            let blob_location = blob_index.get_block_location_ref(&cm.hash).unwrap();
            info!("blob_location: {:?}", blob_location);
        }
    }

    Ok(())
}
