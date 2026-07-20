//! CLI handler for `blu agent`, `blu unlock`, and `blu lock` subcommands.

use crate::agent::biometric;
use crate::agent::AgentClient;
use crate::cli::clapargs::{AgentArgs, AgentCommand};
use crate::cli::passphrase;
use crate::error::BluError;
use crate::keys;
use crate::keys::mnemonic;

/// Dispatch agent subcommands.
pub fn agent(args: AgentArgs) -> Result<(), BluError> {
    match args.command {
        AgentCommand::Status => agent_status(),
        AgentCommand::Stop => agent_stop(),
    }
}

/// Unlock the agent: try biometric first, then fall back to passphrase.
///
/// Neither path requires being inside a blu repository. The identity
/// lives under XDG data home and the agent resolves it directly.
pub fn unlock() -> Result<(), BluError> {
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
/// the PQ seed, and send it to the agent.
fn try_biometric_unlock(client: &AgentClient) -> Result<(), BluError> {
    let seed = biometric::unlock()?;
    let pq_seed = mnemonic::derive_pq_seed(&seed)?;
    let pubkey = client.unlock_with_pq_seed(pq_seed.as_bytes())?;
    println!("unlocked via Touch ID ({})", pubkey);

    Ok(())
}

/// Unlock using the global identity file + passphrase.
///
/// No vault config is needed; the agent resolves the global identity
/// file itself. This means `blu unlock` works from any directory.
fn unlock_with_passphrase(client: &AgentClient) -> Result<(), BluError> {
    // Try without passphrase first (unencrypted key file)
    match client.unlock("") {
        Ok(pubkey) => {
            println!("unlocked ({})", pubkey);
            return Ok(());
        }
        Err(BluError::WrongPassphrase) | Err(BluError::Internal(_)) => {
            // Key is passphrase-protected
        }
        Err(e) => return Err(e),
    }

    // Then the environment; a wrong value here fails rather than prompting
    if let Some(pass) = passphrase::passphrase_from_env() {
        let pubkey = client.unlock(&pass)?;
        println!("unlocked ({})", pubkey);
        return Ok(());
    }

    let pass = keys::prompt_passphrase("Enter passphrase: ", false)?;
    let pubkey = client.unlock(&pass)?;
    println!("unlocked ({})", pubkey);

    Ok(())
}

/// Lock the agent: zeroize all cached keys.
pub fn lock() -> Result<(), BluError> {
    let client = AgentClient::new()?;

    if !client.is_running() {
        println!("agent is not running");
        return Ok(());
    }

    client.lock()?;
    println!("agent locked");
    Ok(())
}

fn agent_status() -> Result<(), BluError> {
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

fn agent_stop() -> Result<(), BluError> {
    let client = AgentClient::new()?;

    if !client.is_running() {
        println!("agent is not running");
        return Ok(());
    }

    client.shutdown()?;
    println!("agent stopped");
    Ok(())
}
