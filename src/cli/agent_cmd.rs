//! CLI handler for `blu agent`, `blu unlock`, and `blu lock` subcommands.

use age::secrecy::ExposeSecret;

use crate::agent::biometric;
use crate::agent::AgentClient;
use crate::cli::clapargs::{AgentArgs, AgentCommand};
use crate::config;
use crate::error::BluError;
use crate::keys;
use crate::keys::mnemonic;

/// Dispatch agent subcommands.
pub fn agent(args: AgentArgs) -> Result<(), Box<dyn std::error::Error>> {
    match args.command {
        AgentCommand::Status => agent_status(),
        AgentCommand::Stop => agent_stop(),
    }
}

/// Unlock the agent: try biometric first, then fall back to passphrase.
///
/// The biometric path does not require being inside a blu repository
/// (identity is global in `~/.blu/`). The passphrase path requires
/// a repository to find the identity file.
pub fn unlock() -> Result<(), Box<dyn std::error::Error>> {
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

    // Try biometric unlock if available
    if biometric::has_biometric_identity() && biometric::is_available() {
        match try_biometric_unlock(&client) {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("Touch ID unlock failed: {}", e);
                eprintln!("Falling back to passphrase...");
            }
        }
    }

    // Fall back to identity file + passphrase
    unlock_with_passphrase(&client)
}

/// Attempt biometric unlock: retrieve seed via Touch ID, derive
/// identity, send secret to agent.
fn try_biometric_unlock(client: &AgentClient) -> Result<(), Box<dyn std::error::Error>> {
    let seed = biometric::unlock().map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    let identity = mnemonic::derive_x25519_identity(&seed)?;

    // Extract the secret key string to send to the agent
    let identity_secret = identity.to_string();
    let secret_str = identity_secret.expose_secret();

    let pubkey = client.unlock_with_secret(secret_str)?;
    println!("unlocked via Touch ID ({})", pubkey);

    Ok(())
}

/// Unlock using the identity file from vault config + passphrase.
fn unlock_with_passphrase(client: &AgentClient) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = config::read_config(".").map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        Box::new(BluError::InvalidConfig(e.to_string())) as Box<dyn std::error::Error>
    })?;

    if cfg.encryption.is_none() {
        return Err(Box::new(BluError::NoKeyConfigured));
    }

    let identity_path = cfg.identity_path()?;
    let identity_str = identity_path.to_string_lossy().to_string();

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

    if let Some(profile) = result["profile"].as_str() {
        println!("profile: {}", profile);
    }
    if let Some(key) = result["public_key"].as_str() {
        println!("public key: {}", key);
    }
    if let Some(remaining) = result["timeout_remaining"].as_str() {
        println!("timeout in: {}", remaining);
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
