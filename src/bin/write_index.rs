#![allow(clippy::uninlined_format_args)]

#[macro_use]
extern crate log;

use clap::Parser;
use simplelog::*;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use blu::age::BlackBox;
use blu::block::PlainIndex;
use blu::io::BlackBoxSerializable;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

#[derive(Parser)]
pub struct Args {
    pub dir: String,
    pub outfile: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    CombinedLogger::init(vec![TermLogger::new(
        LevelFilter::Debug,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )])
    .unwrap();

    info!("Started write_index util");

    let args = Args::parse();
    // move into the basedir for all internal operations, like `git -C <dir>`
    let prev_dir = env::current_dir()?;
    env::set_current_dir(&args.dir)?;
    let dir = Path::new(".");

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    info!("Indexing {}", args.dir);
    let index = PlainIndex::new(dir)?;

    let outfile = match args.outfile {
        Some(val) => PathBuf::from(val),
        None => {
            let index_path = Path::new(&args.dir).join(".blu/indexes/index.dat");
            warn!(
                "warn: no outfile given, using default path {}",
                index_path.display()
            );
            index_path
        }
    };

    // back out here since we pass a filename as a direct path
    env::set_current_dir(prev_dir)?;
    match write_index_file(&index, &bbox, &outfile) {
        Ok(num_bytes) => info!(
            "Index written to {} ({} bytes)",
            outfile.display(),
            num_bytes
        ),
        Err(e) => error!("Error writing index: {}", e),
    }

    Ok(())
}

fn write_index_file<P: AsRef<Path>>(
    index: &PlainIndex,
    bbox: &BlackBox,
    outfile: P,
) -> Result<usize, Box<dyn std::error::Error>> {
    // create parent dir(s) if necessary
    if let Some(parent_dir) = outfile.as_ref().parent() {
        fs::create_dir_all(parent_dir)?;
    }
    let mut enc_idx_bytes = Vec::new();
    index.write(&mut enc_idx_bytes, bbox)?;
    let size = enc_idx_bytes.len();
    let mut file = fs::File::create(outfile)?;
    file.write_all(&enc_idx_bytes)?;
    Ok(size)
}
