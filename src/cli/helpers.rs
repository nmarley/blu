//! Helper functions for CLI commands.

use std::path::Path;

use crate::age::BlackBox;
use crate::config::{self, Config};
use crate::error::{BluError, Result};
use crate::keys;

/// Options for loading the encryption context.
pub struct LoadOptions<'a> {
    /// Passphrase to decrypt the identity file (if encrypted).
    /// If None and the file is encrypted, will prompt interactively.
    pub passphrase: Option<&'a str>,
    /// If true, skip passphrase prompt and fail if key is encrypted.
    pub no_passphrase: bool,
}

impl Default for LoadOptions<'_> {
    fn default() -> Self {
        Self {
            passphrase: None,
            no_passphrase: false,
        }
    }
}

/// Load the config and BlackBox for CLI operations.
///
/// This is the main entry point for CLI commands that need encryption.
pub fn load_config_and_blackbox(opts: &LoadOptions<'_>) -> Result<(Config, BlackBox)> {
    let dir = Path::new(".");

    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        BluError::InvalidConfig(e.to_string())
    })?;

    let bbox = load_blackbox_from_config(&cfg, opts)?;

    Ok((cfg, bbox))
}

/// Load the BlackBox from a config, handling passphrase prompting.
pub fn load_blackbox_from_config(cfg: &Config, opts: &LoadOptions<'_>) -> Result<BlackBox> {
    if !cfg.has_encryption() {
        return Err(BluError::NoKeyConfigured);
    }

    // Try with provided passphrase first
    if let Some(pass) = opts.passphrase {
        return cfg.load_blackbox(Some(pass));
    }

    // Try without passphrase (for unencrypted keys)
    match cfg.load_blackbox(None) {
        Ok(bbox) => return Ok(bbox),
        Err(BluError::PassphraseRequired) if !opts.no_passphrase => {
            // Key is encrypted, need to prompt
        }
        Err(e) => return Err(e),
    }

    // Prompt for passphrase
    let pass = keys::prompt_passphrase("Enter passphrase: ", false)?;
    cfg.load_blackbox(Some(&pass))
}

/// Load just the config (for commands that don't need encryption).
pub fn load_config() -> Result<Config> {
    let dir = Path::new(".");

    config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        BluError::InvalidConfig(e.to_string())
    })
}
