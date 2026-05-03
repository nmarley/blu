//! Client for communicating with the agent daemon.
//!
//! The CLI uses this to connect to the agent socket, send JSON-RPC
//! requests, and read responses. It also handles auto-starting the
//! agent if it is not running.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

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
        UnixStream::connect(&self.paths.socket).is_ok()
    }

    /// Send a JSON-RPC request and return the parsed response.
    ///
    /// Returns the full JSON-RPC response object. Use the typed
    /// convenience methods (unlock, lock, encrypt, decrypt) instead
    /// of calling this directly.
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

        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(10))).ok();

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

        if let Some(err) = response.get("error") {
            let msg = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown agent error");
            let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);

            return Err(match code {
                -32000 => BluError::Internal("agent is locked".into()),
                -32001 => BluError::WrongPassphrase,
                -32002 => {
                    let path = msg.strip_prefix("key file not found: ").unwrap_or(msg);
                    BluError::KeyFileNotFound { path: path.into() }
                }
                _ => BluError::Internal(format!("agent error: {}", msg)),
            });
        }

        Ok(response)
    }

    /// Send a status request to the agent.
    pub fn status(&self) -> Result<serde_json::Value> {
        self.request("status", serde_json::json!({}))
    }

    /// Unlock the agent with a passphrase and identity file path.
    ///
    /// Returns the public key on success.
    pub fn unlock(&self, identity_path: &str, passphrase: &str) -> Result<String> {
        let resp = self.request(
            "unlock",
            serde_json::json!({
                "identity_path": identity_path,
                "passphrase": passphrase,
            }),
        )?;

        let public_key = resp["result"]["public_key"]
            .as_str()
            .ok_or_else(|| BluError::Internal("missing public_key in unlock response".into()))?
            .to_string();

        Ok(public_key)
    }

    /// Unlock the agent with a raw secret key string (AGE-SECRET-KEY-...).
    ///
    /// Used by the biometric unlock path where the identity is derived
    /// from the seed rather than loaded from a file.
    pub fn unlock_with_secret(&self, secret: &str) -> Result<String> {
        let resp = self.request(
            "unlock_with_secret",
            serde_json::json!({ "secret": secret }),
        )?;

        let public_key = resp["result"]["public_key"]
            .as_str()
            .ok_or_else(|| BluError::Internal("missing public_key in unlock response".into()))?
            .to_string();

        Ok(public_key)
    }

    /// Unlock the agent with a raw secret key string and PQ seed.
    ///
    /// Same as `unlock_with_secret`, but also sends the 32-byte PQ
    /// seed (base64-encoded) so the agent can decrypt
    /// mlkem768x25519-wrapped KEKs.
    pub fn unlock_with_secret_pq(&self, secret: &str, pq_seed: &[u8; 32]) -> Result<String> {
        let resp = self.request(
            "unlock_with_secret",
            serde_json::json!({
                "secret": secret,
                "pq_seed": BASE64.encode(pq_seed),
            }),
        )?;

        let public_key = resp["result"]["public_key"]
            .as_str()
            .ok_or_else(|| BluError::Internal("missing public_key in unlock response".into()))?
            .to_string();

        Ok(public_key)
    }

    /// Lock the agent (zeroize all secrets).
    pub fn lock(&self) -> Result<()> {
        self.request("lock", serde_json::json!({}))?;
        Ok(())
    }

    /// Encrypt data via the agent.
    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        let resp = self.request(
            "encrypt",
            serde_json::json!({ "data": BASE64.encode(data) }),
        )?;

        let ciphertext_b64 = resp["result"]["ciphertext"]
            .as_str()
            .ok_or_else(|| BluError::Internal("missing ciphertext in response".into()))?;

        BASE64
            .decode(ciphertext_b64)
            .map_err(|e| BluError::Internal(format!("invalid base64 from agent: {}", e)))
    }

    /// Decrypt data via the agent.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        let resp = self.request(
            "decrypt",
            serde_json::json!({ "data": BASE64.encode(data) }),
        )?;

        let plaintext_b64 = resp["result"]["plaintext"]
            .as_str()
            .ok_or_else(|| BluError::Internal("missing plaintext in response".into()))?;

        BASE64
            .decode(plaintext_b64)
            .map_err(|e| BluError::Internal(format!("invalid base64 from agent: {}", e)))
    }

    /// Generate and wrap a new DEK via the agent.
    ///
    /// Returns `(dek_bytes, wrapped_dek, kek_version)`. The agent
    /// generates a random DEK, wraps it with the cached KEK, and
    /// returns both. If no KEK is loaded yet, `kek_dir` is sent so
    /// the agent can load it on demand.
    pub fn wrap_dek(&self, kek_dir: Option<&str>) -> Result<(Vec<u8>, Vec<u8>, u16)> {
        let mut params = serde_json::json!({});
        if let Some(dir) = kek_dir {
            params["kek_dir"] = serde_json::json!(dir);
        }

        let resp = self.request("wrap_dek", params)?;

        let dek_b64 = resp["result"]["dek"]
            .as_str()
            .ok_or_else(|| BluError::Internal("missing dek in wrap_dek response".into()))?;
        let wrapped_b64 = resp["result"]["wrapped_dek"]
            .as_str()
            .ok_or_else(|| BluError::Internal("missing wrapped_dek in wrap_dek response".into()))?;
        let kek_version = resp["result"]["kek_version"]
            .as_u64()
            .ok_or_else(|| BluError::Internal("missing kek_version in wrap_dek response".into()))?
            as u16;

        let dek = BASE64
            .decode(dek_b64)
            .map_err(|e| BluError::Internal(format!("invalid base64 dek: {}", e)))?;
        let wrapped = BASE64
            .decode(wrapped_b64)
            .map_err(|e| BluError::Internal(format!("invalid base64 wrapped_dek: {}", e)))?;

        Ok((dek, wrapped, kek_version))
    }

    /// Unwrap a DEK via the agent.
    ///
    /// Sends the wrapped DEK and KEK version to the agent, which
    /// unwraps it using the cached KEK and returns the plaintext DEK.
    pub fn unwrap_dek(
        &self,
        wrapped_dek: &[u8],
        kek_version: u16,
        kek_dir: Option<&str>,
    ) -> Result<Vec<u8>> {
        let mut params = serde_json::json!({
            "wrapped_dek": BASE64.encode(wrapped_dek),
            "kek_version": kek_version,
        });
        if let Some(dir) = kek_dir {
            params["kek_dir"] = serde_json::json!(dir);
        }

        let resp = self.request("unwrap_dek", params)?;

        let dek_b64 = resp["result"]["dek"]
            .as_str()
            .ok_or_else(|| BluError::Internal("missing dek in unwrap_dek response".into()))?;

        BASE64
            .decode(dek_b64)
            .map_err(|e| BluError::Internal(format!("invalid base64 dek: {}", e)))
    }

    /// Send a shutdown request to the agent.
    pub fn shutdown(&self) -> Result<()> {
        let _ = self.request("shutdown", serde_json::json!({}));
        Ok(())
    }

    /// Start the agent daemon as a background process.
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

    /// Start a daemon in a background thread, wait for socket, return
    /// the client and join handle.
    fn start_test_client() -> (AgentClient, AgentPaths, std::thread::JoinHandle<()>) {
        let tmp = tempdir().unwrap();
        // Keep the tempdir so it is not removed while the daemon runs.
        // Tests are short-lived so this is fine.
        let tmp_path = tmp.keep();
        let paths = AgentPaths::from_base(&tmp_path).unwrap();
        let paths_for_daemon = paths.clone();

        let handle = std::thread::spawn(move || {
            run_daemon(&paths_for_daemon).unwrap();
        });

        for _ in 0..50 {
            if paths.socket_exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let client = AgentClient::with_paths(paths.clone());
        (client, paths, handle)
    }

    #[test]
    fn client_status_when_locked() {
        let (client, _paths, handle) = start_test_client();

        let resp = client.status().unwrap();
        assert_eq!(resp["result"]["unlocked"], false);

        client.shutdown().unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn client_unlock_lock_cycle() {
        let (client, _paths, handle) = start_test_client();

        let pubkey = client.unlock("test/blu_secrets/blu.key", "unused").unwrap();
        assert!(pubkey.starts_with("age1"));

        let resp = client.status().unwrap();
        assert_eq!(resp["result"]["unlocked"], true);

        client.lock().unwrap();

        let resp = client.status().unwrap();
        assert_eq!(resp["result"]["unlocked"], false);

        client.shutdown().unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn client_encrypt_decrypt() {
        let (client, _paths, handle) = start_test_client();
        client.unlock("test/blu_secrets/blu.key", "unused").unwrap();

        let plaintext = b"agent encrypt/decrypt test data";
        let ciphertext = client.encrypt(plaintext).unwrap();
        assert_ne!(&ciphertext[..], &plaintext[..]);

        let decrypted = client.decrypt(&ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);

        client.shutdown().unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn client_encrypt_when_locked() {
        let (client, _paths, handle) = start_test_client();

        let result = client.encrypt(b"data");
        assert!(result.is_err());

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
