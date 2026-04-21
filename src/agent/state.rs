//! Agent in-memory state: holds the decrypted identity and BlackBox.
//!
//! All secret material is zeroized on drop and when the agent is
//! locked. The BlackBox is constructed from the decrypted identity
//! and provides encrypt/decrypt operations.
//!
//! When a vault's KEK is loaded, the agent can also perform DEK
//! wrap/unwrap operations without exposing the KEK to CLI processes.
//!
//! Timeout tracking: the state records when the agent was unlocked
//! and when the last RPC activity occurred. The daemon polls
//! `check_timeouts()` on each iteration of the accept loop.
//!
//! Memory locking: on unlock, secret buffers are mlocked to prevent
//! the OS from paging them to swap. On lock, they are munlocked
//! before being zeroized. The `age::x25519::Identity` internal
//! Curve25519 scalar cannot be mlocked because it is owned by the
//! `age` crate and does not expose its backing memory.

use std::str::FromStr;
use std::time::{Duration, Instant};

use zeroize::Zeroize;

use crate::age::BlackBox;
use crate::agent::config::AgentConfig;
use crate::agent::memlock;
use crate::error::{BluError, Result};
use crate::keys;
use crate::keys::dek::Dek;
use crate::keys::hybrid_kem::HybridSeed;
use crate::keys::kek::Kek;
use crate::keys::pq::PqIdentity;

/// The agent's mutable state. Holds an optional decrypted identity.
pub struct AgentState {
    /// The decrypted secret key string (AGE-SECRET-KEY-...).
    /// Zeroized when locked or dropped.
    secret_key: Option<SecretString>,
    /// The BlackBox built from the decrypted identity.
    blackbox: Option<BlackBox>,
    /// The public key string (age1...).
    public_key: Option<String>,

    /// Post-quantum identity seed (zeroized on lock/drop).
    pq_seed: Option<HybridSeed>,

    /// Cached KEK for the current vault (zeroized on lock/drop).
    kek: Option<Kek>,
    /// Version of the cached KEK.
    kek_version: u16,

    /// When the agent was last unlocked (None if locked).
    unlocked_at: Option<Instant>,
    /// When the last RPC activity occurred (None if locked).
    last_activity: Option<Instant>,

    /// Timeout configuration.
    config: AgentConfig,

    /// Whether the secret_key heap buffer is currently mlocked.
    secret_key_mlocked: bool,
    /// Whether the KEK bytes are currently mlocked.
    kek_mlocked: bool,
    /// Whether the PQ seed bytes are currently mlocked.
    pq_seed_mlocked: bool,
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
            pq_seed: None,
            kek: None,
            kek_version: 0,
            unlocked_at: None,
            last_activity: None,
            config: AgentConfig::default(),
            secret_key_mlocked: false,
            kek_mlocked: false,
            pq_seed_mlocked: false,
        }
    }

    /// Create a new locked agent state with the given config.
    pub fn with_config(config: AgentConfig) -> Self {
        Self {
            secret_key: None,
            blackbox: None,
            public_key: None,
            pq_seed: None,
            kek: None,
            kek_version: 0,
            unlocked_at: None,
            last_activity: None,
            config,
            secret_key_mlocked: false,
            kek_mlocked: false,
            pq_seed_mlocked: false,
        }
    }

    /// Whether the agent holds a decrypted identity.
    pub fn is_unlocked(&self) -> bool {
        self.blackbox.is_some()
    }

    /// Whether the agent has a cached KEK.
    pub fn has_kek(&self) -> bool {
        self.kek.is_some()
    }

    /// The cached KEK version, if loaded.
    pub fn kek_version(&self) -> Option<u16> {
        self.kek.as_ref().map(|_| self.kek_version)
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

        self.mlock_secret_key();

        let now = Instant::now();
        self.unlocked_at = Some(now);
        self.last_activity = Some(now);

        Ok(public_key)
    }

    /// Unlock the agent with a raw secret key string (AGE-SECRET-KEY-...).
    ///
    /// Used by the biometric unlock path: the CLI derives the identity
    /// from the seed and sends the secret key directly, bypassing the
    /// identity file.
    pub fn unlock_with_secret(&mut self, secret_key_str: &str) -> Result<String> {
        let identity = age::x25519::Identity::from_str(secret_key_str)
            .map_err(|e| BluError::InvalidKeyFormat(e.to_string()))?;

        let public_key = identity.to_public().to_string();
        let bbox = keys::blackbox_from_identity(identity);

        self.secret_key = Some(SecretString {
            inner: secret_key_str.to_string(),
        });
        self.blackbox = Some(bbox);
        self.public_key = Some(public_key.clone());

        self.mlock_secret_key();

        let now = Instant::now();
        self.unlocked_at = Some(now);
        self.last_activity = Some(now);

        Ok(public_key)
    }

    /// Set the PQ seed for this agent session.
    ///
    /// Called after unlock when the seed is available (from BIP39
    /// derivation or biometric recovery). The PQ seed enables
    /// decryption of mlkem768x25519-wrapped KEKs.
    #[allow(dead_code)]
    pub fn set_pq_seed(&mut self, seed: HybridSeed) {
        self.pq_seed = Some(seed);
        self.mlock_pq_seed();
    }

    /// Whether the agent has a PQ identity loaded.
    #[allow(dead_code)]
    pub fn has_pq(&self) -> bool {
        self.pq_seed.is_some()
    }

    /// Lock the agent: munlock and zeroize all secret material.
    pub fn lock(&mut self) {
        // munlock before drop so the pages are unlocked before zeroize
        self.munlock_secret_key();
        self.munlock_pq_seed();
        self.munlock_kek();

        // SecretString::drop will zeroize the key
        self.secret_key.take();
        self.blackbox.take();
        self.public_key.take();
        // HybridSeed::drop will zeroize the PQ seed (ZeroizeOnDrop)
        self.pq_seed.take();
        // Kek::drop will zeroize the KEK (ZeroizeOnDrop)
        self.kek.take();
        self.kek_version = 0;
        self.unlocked_at = None;
        self.last_activity = None;
    }

    /// Load and cache a vault's KEK.
    ///
    /// The `kek_dir` is the path to the vault's `.blu/` directory
    /// (the KekStore lives under `.blu/keys/`). The agent provides
    /// both its PQ identity (for new mlkem768x25519-wrapped KEKs) and
    /// its X25519 identity (for old X25519-wrapped KEKs) so that
    /// either format can be decrypted.
    pub fn load_kek(&mut self, kek_dir: &str) -> Result<u16> {
        let identity_str = self
            .secret_key
            .as_ref()
            .ok_or(BluError::Internal("agent is locked".into()))?;

        let store = keys::kek::KekStore::new(std::path::Path::new(kek_dir));

        // Build a list of identities to try: PQ first, then X25519
        let pq_identity = self.pq_seed.as_ref().map(|s| PqIdentity::new(s.clone()));
        let x25519_identity = age::x25519::Identity::from_str(&identity_str.inner)
            .map_err(|e| BluError::InvalidKeyFormat(e.to_string()))?;

        let mut identities: Vec<&dyn age::Identity> = Vec::new();
        if let Some(ref pq_id) = pq_identity {
            identities.push(pq_id as &dyn age::Identity);
        }
        identities.push(&x25519_identity as &dyn age::Identity);

        let (kek, version) = store.unwrap_current_kek_with(&identities)?;

        self.kek = Some(kek);
        self.kek_version = version;
        self.mlock_kek();

        Ok(version)
    }

    /// Set a KEK directly (for testing or when the KEK is provided
    /// rather than loaded from disk).
    #[allow(dead_code)]
    pub fn set_kek(&mut self, kek: Kek, version: u16) {
        self.kek = Some(kek);
        self.kek_version = version;
        self.mlock_kek();
    }

    /// Lock the secret key's heap buffer into physical memory.
    fn mlock_secret_key(&mut self) {
        if let Some(ref sk) = self.secret_key {
            let ptr = sk.inner.as_ptr();
            let len = sk.inner.capacity();
            if memlock::mlock_slice(ptr, len) {
                memlock::mark_dontdump(ptr, len);
                self.secret_key_mlocked = true;
            }
        }
    }

    /// Unlock the secret key's heap buffer from physical memory.
    fn munlock_secret_key(&mut self) {
        if self.secret_key_mlocked {
            if let Some(ref sk) = self.secret_key {
                memlock::munlock_slice(sk.inner.as_ptr(), sk.inner.capacity());
            }
            self.secret_key_mlocked = false;
        }
    }

    /// Lock the PQ seed bytes into physical memory.
    fn mlock_pq_seed(&mut self) {
        if let Some(ref seed) = self.pq_seed {
            let bytes = seed.as_bytes();
            let ptr = bytes.as_ptr();
            let len = bytes.len();
            if memlock::mlock_slice(ptr, len) {
                memlock::mark_dontdump(ptr, len);
                self.pq_seed_mlocked = true;
            }
        }
    }

    /// Unlock the PQ seed bytes from physical memory.
    fn munlock_pq_seed(&mut self) {
        if self.pq_seed_mlocked {
            if let Some(ref seed) = self.pq_seed {
                let bytes = seed.as_bytes();
                memlock::munlock_slice(bytes.as_ptr(), bytes.len());
            }
            self.pq_seed_mlocked = false;
        }
    }

    /// Lock the KEK bytes into physical memory.
    fn mlock_kek(&mut self) {
        if let Some(ref kek) = self.kek {
            let bytes = kek.as_bytes();
            let ptr = bytes.as_ptr();
            let len = bytes.len();
            if memlock::mlock_slice(ptr, len) {
                memlock::mark_dontdump(ptr, len);
                self.kek_mlocked = true;
            }
        }
    }

    /// Unlock the KEK bytes from physical memory.
    fn munlock_kek(&mut self) {
        if self.kek_mlocked {
            if let Some(ref kek) = self.kek {
                let bytes = kek.as_bytes();
                memlock::munlock_slice(bytes.as_ptr(), bytes.len());
            }
            self.kek_mlocked = false;
        }
    }

    /// Generate a new DEK, wrap it with the cached KEK, and return
    /// the plaintext DEK bytes, the wrapped DEK, and the KEK version.
    ///
    /// This is the agent-side implementation of the `wrap_dek` RPC.
    pub fn wrap_dek(&self) -> Result<(Vec<u8>, Vec<u8>, u16)> {
        let kek = self
            .kek
            .as_ref()
            .ok_or(BluError::Internal("no KEK loaded".into()))?;

        let dek = Dek::generate();
        let wrapped = dek.wrap(kek)?;
        let dek_bytes = dek.as_bytes().to_vec();

        Ok((dek_bytes, wrapped, self.kek_version))
    }

    /// Unwrap a DEK using the cached KEK.
    ///
    /// This is the agent-side implementation of the `unwrap_dek` RPC.
    pub fn unwrap_dek(&self, wrapped_dek: &[u8], kek_version: u16) -> Result<Vec<u8>> {
        let kek = self
            .kek
            .as_ref()
            .ok_or(BluError::Internal("no KEK loaded".into()))?;

        if kek_version != self.kek_version {
            return Err(BluError::DecryptionFailed(format!(
                "KEK version mismatch: requested v{}, agent has v{}",
                kek_version, self.kek_version
            )));
        }

        let dek = Dek::unwrap(kek, wrapped_dek)?;
        Ok(dek.as_bytes().to_vec())
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
    use rand::RngCore;

    const TEST_KEY_PATH: &str = "test/blu_secrets/blu.key";

    #[test]
    fn new_state_is_locked() {
        let state = AgentState::new();
        assert!(!state.is_unlocked());
        assert!(!state.has_kek());
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
        assert!(!state.has_kek());
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

    #[test]
    fn wrap_dek_without_kek_fails() {
        let mut state = AgentState::new();
        state.unlock(TEST_KEY_PATH, "unused").unwrap();
        assert!(state.wrap_dek().is_err());
    }

    #[test]
    fn unwrap_dek_without_kek_fails() {
        let mut state = AgentState::new();
        state.unlock(TEST_KEY_PATH, "unused").unwrap();
        assert!(state.unwrap_dek(b"fake", 0).is_err());
    }

    #[test]
    fn wrap_unwrap_dek_round_trip() {
        let mut state = AgentState::new();
        state.unlock(TEST_KEY_PATH, "unused").unwrap();

        let kek = Kek::generate();
        state.set_kek(kek, 0);

        let (dek_bytes, wrapped, version) = state.wrap_dek().unwrap();
        assert_eq!(version, 0);
        assert_eq!(dek_bytes.len(), 32);
        assert!(!wrapped.is_empty());

        let unwrapped = state.unwrap_dek(&wrapped, 0).unwrap();
        assert_eq!(unwrapped, dek_bytes);
    }

    #[test]
    fn unwrap_dek_version_mismatch() {
        let mut state = AgentState::new();
        state.unlock(TEST_KEY_PATH, "unused").unwrap();

        let kek = Kek::generate();
        state.set_kek(kek, 0);

        let (_dek_bytes, wrapped, _version) = state.wrap_dek().unwrap();

        // Try to unwrap with wrong version
        let result = state.unwrap_dek(&wrapped, 1);
        assert!(result.is_err());
    }

    #[test]
    fn lock_clears_kek() {
        let mut state = AgentState::new();
        state.unlock(TEST_KEY_PATH, "unused").unwrap();

        let kek = Kek::generate();
        state.set_kek(kek, 5);
        assert!(state.has_kek());
        assert_eq!(state.kek_version(), Some(5));

        state.lock();
        assert!(!state.has_kek());
        assert_eq!(state.kek_version(), None);
    }

    #[test]
    fn unlock_with_secret_key_string() {
        let mut state = AgentState::new();
        let secret = include_str!("../../test/blu_secrets/blu.key").trim();

        let pubkey = state.unlock_with_secret(secret).unwrap();
        assert!(state.is_unlocked());
        assert!(pubkey.starts_with("age1"));

        // Should be able to encrypt/decrypt
        let plaintext = b"secret data";
        let ciphertext = state.encrypt(plaintext).unwrap();
        let decrypted = state.decrypt(&ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn unlock_with_secret_invalid_key() {
        let mut state = AgentState::new();
        let result = state.unlock_with_secret("not-a-valid-key");
        assert!(result.is_err());
        assert!(!state.is_unlocked());
    }

    #[test]
    fn set_pq_seed_and_lock_clears() {
        let mut state = AgentState::new();
        let secret = include_str!("../../test/blu_secrets/blu.key").trim();
        state.unlock_with_secret(secret).unwrap();

        let mut seed_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed_bytes);
        state.set_pq_seed(HybridSeed::new(seed_bytes));
        assert!(state.has_pq());

        state.lock();
        assert!(!state.has_pq());
        assert!(!state.is_unlocked());
    }

    #[test]
    fn load_kek_with_pq_identity() {
        use crate::keys::hybrid_kem::public_key_from_seed;
        use crate::keys::pq::PqRecipient;

        let mut state = AgentState::new();
        let secret = include_str!("../../test/blu_secrets/blu.key").trim();
        state.unlock_with_secret(secret).unwrap();

        // Create a PQ seed and set it
        let mut seed_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed_bytes);
        let seed = HybridSeed::new(seed_bytes);
        let recipient = PqRecipient::new(public_key_from_seed(&seed));
        state.set_pq_seed(seed);

        // Create a temp KEK store with a PQ-wrapped KEK
        let tmp = tempfile::tempdir().unwrap();
        let blu_dir = tmp.path().join(".blu");
        std::fs::create_dir_all(&blu_dir).unwrap();

        let store = keys::kek::KekStore::new(&blu_dir);
        let recipient_str = recipient.to_string();
        let expected_kek = store
            .init_with(&[&recipient as &dyn age::Recipient], &[recipient_str])
            .unwrap();

        // load_kek should succeed using the PQ identity
        let version = state.load_kek(blu_dir.to_str().unwrap()).unwrap();
        assert_eq!(version, 0);
        assert!(state.has_kek());

        // Verify it's the right KEK by doing a DEK round-trip
        let (dek_bytes, wrapped, _) = state.wrap_dek().unwrap();
        let unwrapped = state.unwrap_dek(&wrapped, version).unwrap();
        assert_eq!(unwrapped, dek_bytes);

        // Also verify by directly checking KEK bytes
        let dek = Dek::generate();
        let wrapped_dek = dek.wrap(&expected_kek).unwrap();
        let recovered = Dek::unwrap(&expected_kek, &wrapped_dek).unwrap();
        assert_eq!(recovered.as_bytes(), dek.as_bytes());
    }
}
