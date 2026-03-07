//! Agent in-memory state: holds the decrypted identity and BlackBox.
//!
//! All secret material is zeroized on drop and when the agent is
//! locked. The BlackBox is constructed from the decrypted identity
//! and provides encrypt/decrypt operations.
//!
//! Timeout tracking: the state records when the agent was unlocked
//! and when the last RPC activity occurred. The daemon polls
//! `check_timeouts()` on each iteration of the accept loop.

use std::time::{Duration, Instant};

use zeroize::Zeroize;

use crate::age::BlackBox;
use crate::agent::config::AgentConfig;
use crate::error::{BluError, Result};
use crate::keys;

/// The agent's mutable state. Holds an optional decrypted identity.
pub struct AgentState {
    /// The decrypted secret key string (AGE-SECRET-KEY-...).
    /// Zeroized when locked or dropped.
    secret_key: Option<SecretString>,
    /// The BlackBox built from the decrypted identity.
    blackbox: Option<BlackBox>,
    /// The public key string (age1...).
    public_key: Option<String>,

    /// When the agent was last unlocked (None if locked).
    unlocked_at: Option<Instant>,
    /// When the last RPC activity occurred (None if locked).
    last_activity: Option<Instant>,

    /// Timeout configuration.
    config: AgentConfig,
}

/// A string that zeroizes its contents on drop.
struct SecretString {
    inner: String,
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.inner.zeroize();
    }
}

impl AgentState {
    /// Create a new locked agent state with default config.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            secret_key: None,
            blackbox: None,
            public_key: None,
            unlocked_at: None,
            last_activity: None,
            config: AgentConfig::default(),
        }
    }

    /// Create a new locked agent state with the given config.
    pub fn with_config(config: AgentConfig) -> Self {
        Self {
            secret_key: None,
            blackbox: None,
            public_key: None,
            unlocked_at: None,
            last_activity: None,
            config,
        }
    }

    /// Whether the agent holds a decrypted identity.
    pub fn is_unlocked(&self) -> bool {
        self.blackbox.is_some()
    }

    /// The public key, if unlocked.
    pub fn public_key(&self) -> Option<&str> {
        self.public_key.as_deref()
    }

    /// The timeout profile.
    pub fn profile(&self) -> &AgentConfig {
        &self.config
    }

    /// Record that an RPC was handled (resets idle timer).
    pub fn touch(&mut self) {
        if self.is_unlocked() {
            self.last_activity = Some(Instant::now());
        }
    }

    /// Check whether either timeout has expired. If so, lock
    /// and return true.
    pub fn check_timeouts(&mut self) -> bool {
        if !self.is_unlocked() {
            return false;
        }

        let now = Instant::now();

        // Max timeout: unconditional since unlock
        if let Some(unlocked_at) = self.unlocked_at {
            if now.duration_since(unlocked_at) >= self.config.timeout_max {
                info!("max timeout reached, locking agent");
                self.lock();
                return true;
            }
        }

        // Idle timeout: since last activity
        if let Some(last_activity) = self.last_activity {
            if now.duration_since(last_activity) >= self.config.timeout_idle {
                info!("idle timeout reached, locking agent");
                self.lock();
                return true;
            }
        }

        false
    }

    /// Compute the time remaining until the next timeout fires.
    /// Returns None if the agent is locked.
    pub fn time_remaining(&self) -> Option<Duration> {
        if !self.is_unlocked() {
            return None;
        }

        let now = Instant::now();

        let max_remaining = self.unlocked_at.map(|at| {
            self.config
                .timeout_max
                .saturating_sub(now.duration_since(at))
        });

        let idle_remaining = self.last_activity.map(|at| {
            self.config
                .timeout_idle
                .saturating_sub(now.duration_since(at))
        });

        match (max_remaining, idle_remaining) {
            (Some(m), Some(i)) => Some(m.min(i)),
            (Some(m), None) => Some(m),
            (None, Some(i)) => Some(i),
            (None, None) => None,
        }
    }

    /// Unlock the agent by decrypting an identity file.
    pub fn unlock(&mut self, identity_path: &str, passphrase: &str) -> Result<String> {
        let identity = keys::load_identity(identity_path, Some(passphrase))?;

        // Extract the secret key string
        let identity_secret = identity.to_string();
        let secret_str = age::secrecy::ExposeSecret::expose_secret(&identity_secret);
        let secret_owned = secret_str.to_string();

        // Derive the public key
        let public_key = identity.to_public().to_string();

        // Build the BlackBox
        let bbox = keys::blackbox_from_identity(identity);

        self.secret_key = Some(SecretString {
            inner: secret_owned,
        });
        self.blackbox = Some(bbox);
        self.public_key = Some(public_key.clone());

        let now = Instant::now();
        self.unlocked_at = Some(now);
        self.last_activity = Some(now);

        Ok(public_key)
    }

    /// Lock the agent: zeroize and drop all secret material.
    pub fn lock(&mut self) {
        // SecretString::drop will zeroize the key
        self.secret_key.take();
        self.blackbox.take();
        self.public_key.take();
        self.unlocked_at = None;
        self.last_activity = None;
    }

    /// Encrypt data using the cached BlackBox.
    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        let bbox = self
            .blackbox
            .as_ref()
            .ok_or(BluError::Internal("agent is locked".into()))?;
        bbox.encrypt(data)
            .map_err(|e| BluError::EncryptionFailed(e.to_string()))
    }

    /// Decrypt data using the cached BlackBox.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        let bbox = self
            .blackbox
            .as_ref()
            .ok_or(BluError::Internal("agent is locked".into()))?;
        bbox.decrypt(data)
            .map_err(|e| BluError::DecryptionFailed(e.to_string()))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const TEST_KEY_PATH: &str = "test/blu_secrets/blu.key";

    #[test]
    fn new_state_is_locked() {
        let state = AgentState::new();
        assert!(!state.is_unlocked());
        assert!(state.public_key().is_none());
        assert!(state.time_remaining().is_none());
    }

    #[test]
    fn unlock_plaintext_key() {
        let mut state = AgentState::new();
        let result = state.unlock(TEST_KEY_PATH, "unused");
        assert!(result.is_ok());
        assert!(state.is_unlocked());
        assert!(state.public_key().is_some());
        let pubkey = state.public_key().unwrap();
        assert!(pubkey.starts_with("age1"));
        assert!(state.time_remaining().is_some());
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let mut state = AgentState::new();
        state.unlock(TEST_KEY_PATH, "unused").unwrap();

        let plaintext = b"hello, agent!";
        let ciphertext = state.encrypt(plaintext).unwrap();
        assert_ne!(&ciphertext, plaintext);

        let decrypted = state.decrypt(&ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn encrypt_fails_when_locked() {
        let state = AgentState::new();
        let result = state.encrypt(b"data");
        assert!(result.is_err());
    }

    #[test]
    fn lock_clears_state() {
        let mut state = AgentState::new();
        state.unlock(TEST_KEY_PATH, "unused").unwrap();
        assert!(state.is_unlocked());

        state.lock();
        assert!(!state.is_unlocked());
        assert!(state.public_key().is_none());
        assert!(state.encrypt(b"data").is_err());
        assert!(state.time_remaining().is_none());
    }

    #[test]
    fn idle_timeout_locks_agent() {
        let config = AgentConfig {
            timeout_idle: Duration::from_millis(1),
            timeout_max: Duration::from_secs(3600),
            ..AgentConfig::default()
        };
        let mut state = AgentState::with_config(config);
        state.unlock(TEST_KEY_PATH, "unused").unwrap();
        assert!(state.is_unlocked());

        // Sleep past the idle timeout
        std::thread::sleep(Duration::from_millis(10));
        assert!(state.check_timeouts());
        assert!(!state.is_unlocked());
    }

    #[test]
    fn max_timeout_locks_agent() {
        let config = AgentConfig {
            timeout_idle: Duration::from_secs(3600),
            timeout_max: Duration::from_millis(1),
            ..AgentConfig::default()
        };
        let mut state = AgentState::with_config(config);
        state.unlock(TEST_KEY_PATH, "unused").unwrap();
        assert!(state.is_unlocked());

        std::thread::sleep(Duration::from_millis(10));
        assert!(state.check_timeouts());
        assert!(!state.is_unlocked());
    }

    #[test]
    fn touch_resets_idle_timer() {
        let config = AgentConfig {
            timeout_idle: Duration::from_millis(50),
            timeout_max: Duration::from_secs(3600),
            ..AgentConfig::default()
        };
        let mut state = AgentState::with_config(config);
        state.unlock(TEST_KEY_PATH, "unused").unwrap();

        // Sleep 30ms, then touch (resets idle)
        std::thread::sleep(Duration::from_millis(30));
        state.touch();

        // Sleep another 30ms (total 60ms since unlock, but only 30ms since touch)
        std::thread::sleep(Duration::from_millis(30));
        assert!(!state.check_timeouts());
        assert!(state.is_unlocked());

        // Now sleep past idle
        std::thread::sleep(Duration::from_millis(60));
        assert!(state.check_timeouts());
        assert!(!state.is_unlocked());
    }

    #[test]
    fn check_timeouts_noop_when_locked() {
        let mut state = AgentState::new();
        assert!(!state.check_timeouts());
    }
}
