use std::fs;
use std::io::Write;
use std::path::Path;

use crate::age::BlackBox;
use crate::block::PlainIndex;
use crate::cli::clapargs::InitArgs;
use crate::cli::{check_outfile_writable, write_index_file};
use crate::config;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// initialize the .blu repository
pub fn init(args: InitArgs) -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(&args.dir);
    let abs_path = match std::fs::canonicalize(dir) {
        Ok(dir) => dir,
        Err(e) => {
            return Err(format!("fatal: {}", e).into());
        }
    };

    if config::read_config(dir).is_ok() {
        info!("Config file exists. Nothing to do.");
        return Ok(());
    }

    info!("Config file does not exist. Creating new config file");

    // create .blu dir
    let bludir = abs_path.join(".blu/");
    info!("Initializing new .blu dir in {:?}", bludir);
    fs::create_dir_all(bludir)?;

    // write an empty .blu/config.json file
    let mut file = fs::File::create(dir.join(".blu/config.json"))?;
    let cfg = config::Config::default();

    // TODO: yaml? toml?
    let mut cfg_bytes = serde_json::to_vec_pretty(&cfg)?;
    // Add a newline b/c POSIX and also more tidy and neat. Remember these will
    // be read and edited by humans.
    let _ = cfg_bytes.write(&[0x0a])?;
    file.write_all(&cfg_bytes)?;

    // write an empty index file
    let index_path = dir.join(".blu/indexes/index.dat");
    // test ability to write index file before further processing
    check_outfile_writable(&index_path)?;
    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let index = PlainIndex::new_empty();
    match write_index_file(&index, &bbox, &index_path) {
        Ok(_num_bytes) => info!("Wrote new index to {}", index_path.display()),
        Err(e) => error!("Error writing index: {}", e),
    }

    Ok(())
}
