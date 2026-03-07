//! CLI handler for `blu agent`, `blu unlock`, and `blu lock` subcommands.

use crate::agent::AgentClient;
use crate::cli::clapargs::{AgentArgs, AgentCommand};
use crate::config;
use crate::error::BluError;
use crate::keys;

/// Dispatch agent subcommands.
pub fn agent(args: AgentArgs) -> Result<(), Box<dyn std::error::Error>> {
    match args.command {
        AgentCommand::Status => agent_status(),
        AgentCommand::Stop => agent_stop(),
    }
}

/// Unlock the agent: start if needed, prompt for passphrase, cache key.
///
/// This command does not require being inside a blu repository because
/// the identity path can be resolved from the vault config. However,
/// for now we require a repository so we can read the config to find
/// the identity file path.
pub fn unlock() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = config::read_config(".").map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        Box::new(BluError::InvalidConfig(e.to_string())) as Box<dyn std::error::Error>
    })?;

    if cfg.encryption.is_none() {
        return Err(Box::new(BluError::NoKeyConfigured));
    }

    let identity_path = cfg.identity_path()?;
    let identity_str = identity_path.to_string_lossy().to_string();

    let client = AgentClient::new()?;
    client.ensure_running()?;

    // Check if already unlocked
    let resp = client.status()?;
    let unlocked = resp["result"]["unlocked"].as_bool().unwrap_or(false);
    if unlocked {
        if let Some(key) = resp["result"]["public_key"].as_str() {
            println!("agent is already unlocked ({})", key);
        } else {
            println!("agent is already unlocked");
        }
        return Ok(());
    }

    // Try without passphrase first (unencrypted key file)
    match client.unlock(&identity_str, "") {
        Ok(pubkey) => {
            println!("unlocked ({})", pubkey);
            return Ok(());
        }
        Err(BluError::WrongPassphrase) | Err(BluError::Internal(_)) => {
            // Key is passphrase-protected
        }
        Err(e) => return Err(Box::new(e)),
    }

    let pass = keys::prompt_passphrase("Enter passphrase: ", false)?;
    let pubkey = client.unlock(&identity_str, &pass)?;
    println!("unlocked ({})", pubkey);

    Ok(())
}

/// Lock the agent: zeroize all cached keys.
pub fn lock() -> Result<(), Box<dyn std::error::Error>> {
    let client = AgentClient::new()?;

    if !client.is_running() {
        println!("agent is not running");
        return Ok(());
    }

    client.lock()?;
    println!("agent locked");
    Ok(())
}

fn agent_status() -> Result<(), Box<dyn std::error::Error>> {
    let client = AgentClient::new()?;

    if !client.is_running() {
        println!("agent is not running");
        return Ok(());
    }

    let resp = client.status()?;
    let result = &resp["result"];

    let unlocked = result["unlocked"].as_bool().unwrap_or(false);
    let status = if unlocked { "unlocked" } else { "locked" };
    println!("agent is running ({})", status);

    if let Some(key) = result["public_key"].as_str() {
        println!("public key: {}", key);
    }
    if let Some(expires) = result["expires_at"].as_str() {
        println!("expires at: {}", expires);
    }

    Ok(())
}

fn agent_stop() -> Result<(), Box<dyn std::error::Error>> {
    let client = AgentClient::new()?;

    if !client.is_running() {
        println!("agent is not running");
        return Ok(());
    }

    client.shutdown()?;
    println!("agent stopped");
    Ok(())
}
