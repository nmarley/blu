use std::path::Path;

use crate::age::BlackBox;
use crate::cli::clapargs::AddArgs;
use crate::config;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// Add local files to the index
pub async fn add(args: AddArgs) -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(".");
    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    info!("Started add");

    if args.add_paths.is_empty() {
        info!("Aborting, no paths given");
        return Err("no paths given".into());
    }

    let cfg = config::read_config(dir).await.map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;

    let mut plain_index = match cfg.load_plain_index(&bbox) {
        Some(idx) => idx,
        None => return Err("unable to load index".into()),
    };

    // iterate each path
    for p in args.add_paths {
        info!("Adding {:?}", p);
        plain_index.add(p, None)?;
    }

    cfg.write_plain_index(&plain_index, &bbox).await?;

    Ok(())
}
