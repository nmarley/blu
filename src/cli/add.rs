use crate::cli::clapargs::AddArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};

/// Add local files to the index
pub fn add(args: AddArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Started add");

    if args.add_paths.is_empty() {
        info!("Aborting, no paths given");
        return Err("no paths given".into());
    }

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    let mut plain_index = cfg.load_plain_index(&keys)?;

    // iterate each path
    for p in args.add_paths {
        info!("Adding {:?}", p);
        plain_index.add(p, None)?;
    }

    cfg.write_plain_index(&plain_index, &keys)?;

    Ok(())
}
