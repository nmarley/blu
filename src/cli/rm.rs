use std::path::Path;

use crate::age::BlackBox;
use crate::cli::clapargs::RemoveArgs;
use crate::config;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// Remove local files from the index (or remove a path)
pub fn remove(args: RemoveArgs) -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(".");
    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    info!("Started rm");

    if args.paths.is_empty() {
        info!("Aborting, no paths given");
        return Err("no paths given".into());
    }
    if args.hashes.is_empty() {
        info!("Aborting, no hashes given");
        return Err("no hashes given".into());
    }

    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;

    let mut plain_index = match cfg.load_plain_index(&bbox) {
        Some(idx) => idx,
        None => return Err("unable to load index".into()),
    };

    // // iterate each path
    // for p in args.add_paths {
    //     info!("Adding {:?}", p);
    //     plain_index.add(p, None)?;
    // }
    // cfg.write_plain_index(&plain_index, &bbox)?;

    Ok(())
}
