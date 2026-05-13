//! Helper functions for CLI commands.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::agent::AgentClient;
use crate::config::{self, Config};
use crate::dek_provider::DekProvider;
use crate::error::{BluError, Result};
use crate::keys;

/// Global flag for --no-passphrase option.
/// Set by the main binary before calling CLI commands.
static NO_PASSPHRASE: AtomicBool = AtomicBool::new(false);

/// Set the global no-passphrase flag.
pub fn set_no_passphrase(value: bool) {
    NO_PASSPHRASE.store(value, Ordering::SeqCst);
}

/// Get the global no-passphrase flag.
pub fn get_no_passphrase() -> bool {
    NO_PASSPHRASE.load(Ordering::SeqCst)
}

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
            no_passphrase: get_no_passphrase(),
        }
    }
}

/// Load the config and DekProvider for CLI operations.
///
/// This is the main entry point for CLI commands that need encryption.
/// It will try to use the agent daemon for session-cached keys. If the
/// agent is not available (or --no-passphrase is set), it falls back
/// to loading the key directly in-process.
pub fn load_config_and_keys(opts: &LoadOptions<'_>) -> Result<(Config, DekProvider)> {
    let dir = Path::new(".");

    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        BluError::InvalidConfig(e.to_string())
    })?;

    let keys = load_keys_from_config(&cfg, opts)?;

    Ok((cfg, keys))
}

/// Load the DekProvider from a config, handling agent and passphrase prompting.
///
/// Strategy:
/// 1. Always use the agent. PQ-only vaults require the agent-held PQ
///    seed to unwrap KEKs.
/// 2. If --no-passphrase is set, try unlocking with an empty
///    passphrase only and never prompt.
/// 3. Otherwise, connect to the agent (auto-starting if needed),
///    check if already unlocked, prompt and unlock if locked.
pub fn load_keys_from_config(cfg: &Config, opts: &LoadOptions<'_>) -> Result<DekProvider> {
    if !cfg.has_encryption() {
        return Err(BluError::NoKeyConfigured);
    }

    // --no-passphrase: do not prompt, but still use the agent.
    if opts.no_passphrase {
        return load_keys_via_agent(cfg, "");
    }

    // If an explicit passphrase was provided, use the agent with it
    if let Some(pass) = opts.passphrase {
        return load_keys_via_agent(cfg, pass);
    }

    try_agent_keys(cfg)
}

/// Try to get a DekProvider through the agent daemon.
///
/// Connects to the agent (auto-starting if needed), checks status,
/// prompts for passphrase if locked, and returns an agent-backed DekProvider.
fn try_agent_keys(cfg: &Config) -> Result<DekProvider> {
    let client = AgentClient::new()?;
    client.ensure_running()?;
    let kek_dir = Some(cfg.bludir().to_string_lossy().into_owned());

    let resp = client.status()?;
    let unlocked = resp["result"]["unlocked"].as_bool().unwrap_or(false);

    if unlocked {
        return Ok(DekProvider::Agent { client, kek_dir });
    }

    // Agent is running but locked; try without passphrase first
    match client.unlock("") {
        Ok(_) => return Ok(DekProvider::Agent { client, kek_dir }),
        Err(BluError::WrongPassphrase) | Err(BluError::Internal(_)) => {
            // Key is passphrase-protected, need to prompt
        }
        Err(e) => return Err(e),
    }

    let pass = keys::prompt_passphrase("Enter passphrase: ", false)?;
    client.unlock(&pass)?;
    Ok(DekProvider::Agent { client, kek_dir })
}

/// Load a DekProvider via the agent using an explicit passphrase.
fn load_keys_via_agent(cfg: &Config, passphrase: &str) -> Result<DekProvider> {
    let client = AgentClient::new()?;
    client.ensure_running()?;

    client.unlock(passphrase)?;
    let kek_dir = Some(cfg.bludir().to_string_lossy().into_owned());
    Ok(DekProvider::Agent { client, kek_dir })
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
