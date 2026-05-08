//! Helper functions for CLI commands.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::age::BlackBox;
use crate::agent::AgentClient;
use crate::config::{self, Config};
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

/// Load the config and BlackBox for CLI operations.
///
/// This is the main entry point for CLI commands that need encryption.
/// It will try to use the agent daemon for session-cached keys. If the
/// agent is not available (or --no-passphrase is set), it falls back
/// to loading the key directly in-process.
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

/// Load the BlackBox from a config, handling agent and passphrase prompting.
///
/// Strategy:
/// 1. If --no-passphrase is set, try loading the key directly (for
///    unencrypted keys). Skip the agent entirely since we cannot prompt.
/// 2. Otherwise, try the agent path: connect (auto-starting if needed),
///    check if already unlocked, prompt and unlock if locked.
/// 3. If the agent path fails for any reason, fall back to in-process.
pub fn load_blackbox_from_config(cfg: &Config, opts: &LoadOptions<'_>) -> Result<BlackBox> {
    if !cfg.has_encryption() {
        return Err(BluError::NoKeyConfigured);
    }

    // --no-passphrase: skip agent, try direct load with no passphrase
    if opts.no_passphrase {
        return load_blackbox_inprocess(cfg, None);
    }

    // If an explicit passphrase was provided, use the agent with it
    if let Some(pass) = opts.passphrase {
        return load_blackbox_via_agent(cfg, pass);
    }

    // Try the agent path
    match try_agent_blackbox(cfg) {
        Ok(bbox) => return Ok(bbox),
        Err(BluError::WrongPassphrase) => return Err(BluError::WrongPassphrase),
        Err(e) => {
            info!("agent path failed, falling back to in-process: {}", e);
        }
    }

    // Fallback: try without passphrase first (unencrypted key)
    match load_blackbox_inprocess(cfg, None) {
        Ok(bbox) => return Ok(bbox),
        Err(BluError::PassphraseRequired) => {}
        Err(e) => return Err(e),
    }

    // Prompt for passphrase, load in-process
    let pass = keys::prompt_passphrase("Enter passphrase: ", false)?;
    load_blackbox_inprocess(cfg, Some(&pass))
}

/// Try to get a BlackBox through the agent daemon.
///
/// Connects to the agent (auto-starting if needed), checks status,
/// prompts for passphrase if locked, and returns an agent-backed BlackBox.
fn try_agent_blackbox(_cfg: &Config) -> Result<BlackBox> {
    let client = AgentClient::new()?;
    client.ensure_running()?;

    let resp = client.status()?;
    let unlocked = resp["result"]["unlocked"].as_bool().unwrap_or(false);

    if unlocked {
        return Ok(BlackBox::from_agent(client));
    }

    // Agent is running but locked; try without passphrase first
    match client.unlock("") {
        Ok(_) => return Ok(BlackBox::from_agent(client)),
        Err(BluError::WrongPassphrase) | Err(BluError::Internal(_)) => {
            // Key is passphrase-protected, need to prompt
        }
        Err(e) => return Err(e),
    }

    let pass = keys::prompt_passphrase("Enter passphrase: ", false)?;
    client.unlock(&pass)?;
    Ok(BlackBox::from_agent(client))
}

/// Load a BlackBox via the agent using an explicit passphrase.
fn load_blackbox_via_agent(_cfg: &Config, passphrase: &str) -> Result<BlackBox> {
    let client = AgentClient::new()?;
    client.ensure_running()?;

    client.unlock(passphrase)?;
    Ok(BlackBox::from_agent(client))
}

/// Load a BlackBox in-process (the old direct path, no agent).
fn load_blackbox_inprocess(_cfg: &Config, passphrase: Option<&str>) -> Result<BlackBox> {
    let identity_path = keys::global_identity_path()?;
    let identity = keys::load_identity(&identity_path, passphrase)?;
    Ok(keys::blackbox_from_identity(identity))
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
