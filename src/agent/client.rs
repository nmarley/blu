//! Client for communicating with the agent daemon.
//!
//! The CLI uses this to connect to the agent socket, send JSON-RPC
//! requests, and read responses. It also handles auto-starting the
//! agent if it is not running.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

use crate::agent::paths::AgentPaths;
use crate::error::{BluError, Result};

/// Client for the blu agent daemon.
pub struct AgentClient {
    paths: AgentPaths,
}

impl AgentClient {
    /// Create a new client using the default agent paths (`~/.blu/`).
    pub fn new() -> Result<Self> {
        let paths = AgentPaths::resolve()?;
        Ok(Self { paths })
    }

    /// Create a client with explicit paths (for testing).
    pub fn with_paths(paths: AgentPaths) -> Self {
        Self { paths }
    }

    /// Ensure the agent is running, starting it if necessary.
    ///
    /// Returns Ok(()) if the agent is reachable after this call.
    pub fn ensure_running(&self) -> Result<()> {
        if self.is_running() {
            return Ok(());
        }
        self.start_daemon()?;
        self.wait_for_socket(Duration::from_secs(5))
    }

    /// Check whether the agent appears to be running (socket exists
    /// and a process is listening).
    pub fn is_running(&self) -> bool {
        if !self.paths.socket_exists() {
            return false;
        }
        // Try to connect briefly to verify the socket is live
        UnixStream::connect(&self.paths.socket).is_ok()
    }

    /// Send a JSON-RPC request and return the parsed response.
    pub fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        });

        let mut stream = UnixStream::connect(&self.paths.socket).map_err(|e| {
            BluError::Internal(format!(
                "could not connect to agent at {}: {}",
                self.paths.socket.display(),
                e
            ))
        })?;

        // Set a read/write timeout so we don't hang indefinitely
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(10))).ok();

        // Write length-prefixed request
        let body = serde_json::to_vec(&request)?;
        let len = (body.len() as u32).to_be_bytes();
        stream
            .write_all(&len)
            .map_err(|e| BluError::Internal(format!("write to agent: {}", e)))?;
        stream
            .write_all(&body)
            .map_err(|e| BluError::Internal(format!("write to agent: {}", e)))?;
        stream
            .flush()
            .map_err(|e| BluError::Internal(format!("flush to agent: {}", e)))?;

        // Read length-prefixed response
        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .map_err(|e| BluError::Internal(format!("read from agent: {}", e)))?;
        let resp_len = u32::from_be_bytes(len_buf) as usize;

        if resp_len > 64 * 1024 * 1024 {
            return Err(BluError::Internal("agent response too large".into()));
        }

        let mut resp_buf = vec![0u8; resp_len];
        stream
            .read_exact(&mut resp_buf)
            .map_err(|e| BluError::Internal(format!("read from agent: {}", e)))?;

        let response: serde_json::Value = serde_json::from_slice(&resp_buf)?;

        // Check for JSON-RPC error
        if let Some(err) = response.get("error") {
            let msg = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown agent error");
            return Err(BluError::Internal(format!("agent error: {}", msg)));
        }

        Ok(response)
    }

    /// Send a status request to the agent.
    pub fn status(&self) -> Result<serde_json::Value> {
        self.request("status", serde_json::json!({}))
    }

    /// Send a shutdown request to the agent.
    pub fn shutdown(&self) -> Result<()> {
        // Shutdown may close the connection before we read the
        // response, so we tolerate errors here.
        let _ = self.request("shutdown", serde_json::json!({}));
        Ok(())
    }

    /// Start the agent daemon as a background process.
    ///
    /// Spawns `blu __agent-daemon` which forks and daemonizes.
    fn start_daemon(&self) -> Result<()> {
        let exe = std::env::current_exe().map_err(|e| {
            BluError::Internal(format!("could not determine blu executable path: {}", e))
        })?;

        Command::new(&exe)
            .arg("__agent-daemon")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| BluError::Internal(format!("failed to start agent daemon: {}", e)))?;

        Ok(())
    }

    /// Wait for the agent socket to appear, polling at short intervals.
    fn wait_for_socket(&self, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(50);

        while start.elapsed() < timeout {
            if self.is_running() {
                return Ok(());
            }
            std::thread::sleep(poll_interval);
        }

        Err(BluError::Internal(format!(
            "agent did not start within {} seconds",
            timeout.as_secs()
        )))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::agent::daemon::run_daemon;
    use crate::agent::paths::AgentPaths;
    use tempfile::tempdir;

    #[test]
    fn client_status_via_daemon() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        let paths_for_daemon = paths.clone();

        // Start daemon in background thread
        let handle = std::thread::spawn(move || {
            run_daemon(&paths_for_daemon).unwrap();
        });

        // Wait for socket
        for _ in 0..50 {
            if paths.socket_exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let client = AgentClient::with_paths(paths);

        // Status
        let resp = client.status().unwrap();
        assert_eq!(resp["result"]["unlocked"], false);

        // Shutdown
        client.shutdown().unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn is_running_returns_false_when_no_socket() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        let client = AgentClient::with_paths(paths);
        assert!(!client.is_running());
    }
}
