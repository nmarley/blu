//! Agent in-memory state: holds the decrypted identity and BlackBox.
//!
//! All secret material is zeroized on drop and when the agent is
//! locked. The BlackBox is constructed from the decrypted identity
//! and provides encrypt/decrypt operations.

use zeroize::Zeroize;

use crate::age::BlackBox;
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
    /// Create a new locked agent state.
    pub fn new() -> Self {
        Self {
            secret_key: None,
            blackbox: None,
            public_key: None,
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

        Ok(public_key)
    }

    /// Lock the agent: zeroize and drop all secret material.
    pub fn lock(&mut self) {
        // SecretString::drop will zeroize the key
        self.secret_key.take();
        self.blackbox.take();
        self.public_key.take();
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
    }

    #[test]
    fn unlock_plaintext_key() {
        let mut state = AgentState::new();
        // The test key is not passphrase-protected, but
        // load_identity with an empty passphrase on an unencrypted
        // key just ignores the passphrase since the file does not
        // start with "age-encryption.org".
        // For plaintext keys, we pass a dummy passphrase; load_identity
        // detects the file is not encrypted and ignores it.
        let result = state.unlock(TEST_KEY_PATH, "unused");
        assert!(result.is_ok());
        assert!(state.is_unlocked());
        assert!(state.public_key().is_some());
        let pubkey = state.public_key().unwrap();
        assert!(pubkey.starts_with("age1"));
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
    }
}
