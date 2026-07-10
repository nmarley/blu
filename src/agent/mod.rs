//! Agent daemon for blu session management.
//!
//! The agent holds decrypted key material in memory so users only need
//! to enter their passphrase once per session. CLI commands communicate
//! with the agent over a Unix domain socket using length-prefixed
//! JSON-RPC 2.0 messages.
//!
//! # Architecture
//!
//! The agent runs as a background daemon (forked from the main `blu`
//! binary via `blu __agent-daemon`). It listens on a Unix socket under
//! `$XDG_RUNTIME_DIR/blu/` (state-dir fallback) and writes its PID under
//! `$XDG_STATE_HOME/blu/`.
//!
//! The CLI auto-starts the agent on first use and communicates via
//! the socket for all crypto operations.

/// Biometric (Touch ID) unlock support.
pub mod biometric;
mod client;
mod config;
mod daemon;
mod dek_provider_test;
mod memlock;
mod paths;
mod protocol;
mod state;

pub use client::AgentClient;
pub use config::AgentConfig;
pub use daemon::run_daemon;
pub use paths::AgentPaths;
