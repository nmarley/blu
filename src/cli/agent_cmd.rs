//! CLI handler for `blu agent` subcommands.

use crate::agent::AgentClient;
use crate::cli::clapargs::{AgentArgs, AgentCommand};

/// Dispatch agent subcommands.
pub fn agent(args: AgentArgs) -> Result<(), Box<dyn std::error::Error>> {
    match args.command {
        AgentCommand::Status => agent_status(),
        AgentCommand::Stop => agent_stop(),
    }
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
