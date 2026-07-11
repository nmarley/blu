use crate::cli::clapargs::AddArgs;
use crate::cli::helpers::{load_config_and_keys, push_indexes_or_fail, LoadOptions};
use crate::error::BluError;

/// Add local files to the index
pub async fn add(args: AddArgs) -> Result<(), BluError> {
    info!("Started add");

    if args.add_paths.is_empty() {
        info!("Aborting, no paths given");
        return Err(BluError::Internal("no paths given".into()));
    }

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    let mut plain_index = cfg.load_plain_index_or_default(&keys);

    // iterate each path
    for p in args.add_paths {
        info!("Adding {:?}", p);
        plain_index.add(p, None)?;
    }

    cfg.write_plain_index(&plain_index, &keys)?;

    // Sync the updated index to the backend so the source of truth is
    // never behind the local working copy.
    push_indexes_or_fail(&cfg, &keys, None, None).await?;

    Ok(())
}
