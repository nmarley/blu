#![allow(clippy::uninlined_format_args)]

#[macro_use]
extern crate log;

use clap::Parser;
use simplelog::*;
use std::env;
use std::path::Path;

use blu::age::BlackBox;
use blu::config;
// use blu::io::BlackBoxSerializable;
use blu::search::SearchIndex;
// use blu::tagger::TagIndex;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

#[derive(Parser)]
pub struct Args {
    pub dir: String,
    pub needle: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    CombinedLogger::init(vec![TermLogger::new(
        LevelFilter::Info,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )])
    .unwrap();

    info!("Started search_files util");

    let args = Args::parse();
    // move into the basedir for all internal operations, like `git -C <dir>`
    let _prev_dir = env::current_dir()?;
    env::set_current_dir(&args.dir)?;
    let dir = Path::new(".");

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;

    // TODO: load search index here ... (once implemented)
    //   for now, just create a new one every time and then search
    let index = cfg.load_plain_index(&bbox).unwrap();
    // dbg!(&index);
    let mut search_index = SearchIndex::new();

    let files_map = index.files_map_ref();
    for (file_hash, file_ref) in files_map {
        info!("file_hash: {}", file_hash.dbg_short(7));
        for path in file_ref.paths.iter() {
            search_index.add_filename(path.to_str().unwrap(), file_hash);
        }
    }

    let search_result = search_index.search(&args.needle);
    info!("Got {} result(s)", search_result.len());

    for file_hash in search_result.iter() {
        let file_ref = files_map.get(file_hash).unwrap();
        info!("{:?}", file_ref);
    }

    Ok(())
}
