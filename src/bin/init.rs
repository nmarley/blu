#![allow(clippy::uninlined_format_args)]

#[macro_use]
extern crate log;

use clap::Parser;
use simplelog::*;
use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;

use blu::config;

#[derive(Parser)]
pub struct Args {
    pub dir: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    CombinedLogger::init(vec![TermLogger::new(
        LevelFilter::Debug,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )])
    .unwrap();

    let args = Args::parse();

    // move into the basedir for all operations, like `git -C <dir>`
    let dir_arg = args.dir;
    env::set_current_dir(&dir_arg)?;
    let dir = Path::new(".");

    if config::read_config(dir).is_ok() {
        info!("Config file exists. Nothing to do.");
        return Ok(());
    }

    info!("Config file does not exist. Creating new config file");

    info!("Initializing new .blu dir in {:?}", dir_arg);
    // create .blu + .blu/data dirs
    fs::create_dir_all(dir.join(".blu/data"))?;

    // write an empty .blu/config.json file
    let mut file = fs::File::create(dir.join(".blu/config.json"))?;
    file.write_all(b"{}")?;

    // mkdir .blu
    // mkdir .blu/data
    // touch .blu/config.json <-- ??

    Ok(())
}
