use std::fs;
use std::io::Write;
use std::path::Path;

use crate::cli::clapargs::InitArgs;
use crate::config;

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
    // dbg!(&cfg);

    let mut cfg_bytes = serde_json::to_vec_pretty(&cfg)?;
    // Add a newline b/c POSIX and also more tidy and neat. Remember these will
    // be read and edited by humans.
    let _ = cfg_bytes.write(&[0x0a])?;
    file.write_all(&cfg_bytes)?;

    info!("Wrote new .blu/config.json file");

    Ok(())
}
