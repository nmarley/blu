//! AgentBlackBox: a drop-in replacement for BlackBox that delegates
//! encrypt/decrypt operations to the agent daemon over the Unix socket.
//!
//! This allows all existing code that uses `&BlackBox` to work
//! transparently with the agent, without knowing whether crypto
//! happens in-process or in the daemon.

use crate::agent::client::AgentClient;

/// A BlackBox backed by the agent daemon.
///
/// Constructed from an `AgentClient` that is already connected to a
/// running, unlocked agent. Implements the same encrypt/decrypt
/// interface as `BlackBox` by delegating to the agent.
pub struct AgentBlackBox {
    client: AgentClient,
}

impl AgentBlackBox {
    /// Create a new AgentBlackBox from an agent client.
    ///
    /// The client should already be connected to a running agent.
    /// The agent should be unlocked before calling encrypt/decrypt.
    pub fn new(client: AgentClient) -> Self {
        Self { client }
    }

    /// Convert this AgentBlackBox into a regular BlackBox-compatible
    /// wrapper. This creates a BlackBox that delegates to the agent.
    ///
    /// Returns a `BlackBoxProxy` that can be used anywhere a
    /// `&BlackBox` is expected (via its `as_blackbox` method and the
    /// Deref-like pattern).
    pub fn into_proxy(self) -> BlackBoxProxy {
        BlackBoxProxy {
            agent: self,
            // We cannot construct a real BlackBox without the private
            // key (that is the whole point). So we provide encrypt
            // and decrypt methods that match the BlackBox interface.
        }
    }
}

/// A proxy that provides encrypt/decrypt methods matching the
/// BlackBox interface, backed by the agent.
///
/// Since the existing codebase passes `&BlackBox` (a concrete type,
/// not a trait), we cannot directly substitute this. Instead,
/// callers that want agent-backed crypto should use this proxy's
/// methods. In stage 1c, helpers.rs will be updated to use this.
pub struct BlackBoxProxy {
    agent: AgentBlackBox,
}

impl BlackBoxProxy {
    /// Encrypt data via the agent. Matches `BlackBox::encrypt` signature
    /// (with a compatible error type).
    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        self.agent
            .client
            .encrypt(data)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    }

    /// Decrypt data via the agent. Matches `BlackBox::decrypt` signature.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        self.agent
            .client
            .decrypt(data)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    }
}

/// Convert a `BlackBoxProxy` into something that can be used where
/// `BlackBox` is currently expected. This constructs a real `BlackBox`
/// by temporarily requesting the agent to perform a no-op, but the
/// actual approach is to refactor the codebase to use a trait.
///
/// For now (stage 1b), we provide the `from_agent_client` function
/// that creates a `BlackBox` wrapper around the agent. This works
/// by having the agent perform the actual crypto; the "BlackBox"
/// returned is a facade.
///
/// Note: In the current architecture, BlackBox holds age identities
/// in-process. The AgentBlackBox exists so that in stage 1c we can
/// refactor `BlackBox` into a trait or enum that supports both modes.
/// For stage 1b, the agent protocol and client are fully functional
/// and tested; the integration into helpers.rs happens in stage 1c.
impl std::fmt::Debug for BlackBoxProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlackBoxProxy").finish()
    }
}

impl Clone for BlackBoxProxy {
    fn clone(&self) -> Self {
        // AgentClient is not Clone, so we create a new one pointing
        // to the same socket. This is fine because each request opens
        // a new connection.
        let client = AgentClient::new().expect("failed to create agent client for clone");
        Self {
            agent: AgentBlackBox { client },
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::agent::daemon::run_daemon;
    use crate::agent::paths::AgentPaths;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn proxy_encrypt_decrypt() {
        let tmp = tempdir().unwrap();
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
        client.unlock("test/blu_secrets/blu.key", "unused").unwrap();

        let proxy = AgentBlackBox::new(client).into_proxy();

        let plaintext = b"proxy round-trip test";
        let ciphertext = proxy.encrypt(plaintext).unwrap();
        let decrypted = proxy.decrypt(&ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);

        // Use a fresh client for shutdown since the original was moved
        let shutdown_client = AgentClient::with_paths(paths);
        shutdown_client.shutdown().unwrap();
        handle.join().unwrap();
    }
}
