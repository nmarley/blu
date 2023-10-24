#![allow(clippy::uninlined_format_args)]

use clap::Parser;
use simplelog::*;

use blu::cli::{self, clapargs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    CombinedLogger::init(vec![TermLogger::new(
        LevelFilter::Debug,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )])
    .unwrap();

    let args = clapargs::Args::parse();
    dbg!(&args);

    // TODO: Should key(s) be read and stored here in some kind of state or context?
    // use crate::age::BlackBox;
    // const TEST_AGE_SECRET_KEY: &str = include_str!("../test/blu_secrets/blu.key");

    // not yet ready:
    // search_files.rs
    // delete_files.rs
    // defrag_blobs.rs

    #[allow(unreachable_patterns)]
    match args.action {
        clapargs::Action::Init(a) => cli::init(a),
        clapargs::Action::WriteIndex(a) => cli::write_index(a),
        clapargs::Action::EncryptFiles(a) => cli::encrypt_files(a),
        clapargs::Action::RestoreFiles(a) => cli::restore_files(a),
        clapargs::Action::ListFiles(a) => cli::list_files(a),
        clapargs::Action::Tagger(a) => cli::tagger(a),
        clapargs::Action::ReadIndex(a) => cli::read_index(a),
        clapargs::Action::DebugIndex(a) => cli::debug_index(a),
        _ => {
            unimplemented!();
        }
    }
}
