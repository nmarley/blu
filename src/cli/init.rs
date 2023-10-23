use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::cli::clapargs::InitArgs;
use crate::config;

/// initialize the .blu repository
pub fn init(args: InitArgs) -> Result<(), Box<dyn std::error::Error>> {
    // move into the basedir for all operations, like `git -C <dir>`
    let dir_arg = args.dir;
    dbg!(&dir_arg);

    env::set_current_dir(&dir_arg)?;
    let dir = Path::new(".");

    if config::read_config(dir).is_ok() {
        info!("Config file exists. Nothing to do.");
        return Ok(());
    }

    info!("Config file does not exist. Creating new config file");

    info!("Initializing new .blu dir in {:?}", dir_arg);
    // create .blu dir
    fs::create_dir_all(dir.join(".blu/"))?;

    // write an empty .blu/config.json file
    let mut file = fs::File::create(dir.join(".blu/config.json"))?;

    let cfg = config::Config::default();
    // dbg!(&cfg);

    let mut cfg_bytes = serde_json::to_vec_pretty(&cfg)?;
    // Add a newline b/c POSIX and also more tidy and neat. Remember these will
    // be read and edited by humans.
    let _ = cfg_bytes.write(&[0x0a])?;
    file.write_all(&cfg_bytes)?;

    info!("Wrote new .blu/config.json file");

    Ok(())
}
