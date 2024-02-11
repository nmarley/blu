use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::age::BlackBox;
use crate::block::PlainIndex;
use crate::cli::clapargs::InitArgs;
use crate::cli::{check_outfile_writable, write_index_file};
use crate::config;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// initialize the .blu repository
pub async fn init(args: InitArgs) -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(&args.dir);
    let abs_path = match tokio::fs::canonicalize(dir).await {
        Ok(dir) => dir,
        Err(e) => {
            return Err(format!("fatal: {}", e).into());
        }
    };

    // TODO: await/async config module
    if config::read_config(dir).await.is_ok() {
        info!("Config file exists. Nothing to do.");
        return Ok(());
    }

    info!("Config file does not exist. Creating new config file");

    // create .blu dir
    let bludir = abs_path.join(".blu/");
    info!("Initializing new .blu dir in {:?}", bludir);
    fs::create_dir_all(bludir).await?;

    // write an empty .blu/config.toml file
    let mut file = fs::File::create(dir.join(".blu/config.toml")).await?;
    let cfg = config::Config::default();

    let cfg_bytes = toml::to_string_pretty(&cfg)?.into_bytes();
    file.write_all(&cfg_bytes).await?;

    // write an empty index file
    let index_path = dir.join(".blu/indexes/index.dat");
    // test ability to write index file before further processing
    check_outfile_writable(&index_path).await?;
    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let index = PlainIndex::new_empty();
    match write_index_file(&index, &bbox, &index_path).await {
        Ok(_num_bytes) => info!("Wrote new index to {}", index_path.display()),
        Err(e) => error!("Error writing index: {}", e),
    }

    Ok(())
}
