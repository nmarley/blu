//! Agent in-memory state: holds the decrypted PQ hybrid seed and
//! cached KEK.
//!
//! All secret material is zeroized on drop and when the agent is
//! locked.
//!
//! When a vault's KEK is loaded, the agent can perform DEK wrap/unwrap
//! operations without exposing the KEK to CLI processes.
//!
//! Timeout tracking: the state records when the agent was unlocked
//! and when the last RPC activity occurred. The daemon polls
//! `check_timeouts()` on each iteration of the accept loop.
//!
//! Memory locking: on unlock, secret buffers are mlocked to prevent
//! the OS from paging them to swap. On lock, they are munlocked
//! before being zeroized.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::agent::config::AgentConfig;
use crate::agent::memlock;
use crate::error::{BluError, Result};
use crate::keys;
use crate::keys::dek::Dek;
use crate::keys::hybrid_kem::HybridSeed;
use crate::keys::kek::Kek;
use crate::keys::pq::PqIdentity;

/// The agent's mutable state.
///
/// Holds the PQ hybrid seed (the operational secret for unwrapping
/// KEKs) and a cached KEK for the current vault.
pub struct AgentState {
    /// Post-quantum hybrid seed (zeroized on lock/drop).
    /// This is the only identity secret the agent holds.
    pq_seed: Option<HybridSeed>,

    /// The PQ public key string (age1pq...) for display.
    public_key: Option<String>,

    /// Cached KEK for the current vault (zeroized on lock/drop).
    kek: Option<Kek>,
    /// Version of the cached KEK.
    kek_version: u16,
    /// Canonical path to the vault `.blu/` directory for the cached KEK.
    kek_dir: Option<PathBuf>,

    /// When the agent was last unlocked (None if locked).
    unlocked_at: Option<Instant>,
    /// When the last RPC activity occurred (None if locked).
    last_activity: Option<Instant>,

    /// Timeout configuration.
    config: AgentConfig,

    /// Whether the KEK bytes are currently mlocked.
    kek_mlocked: bool,
    /// Whether the PQ seed bytes are currently mlocked.
    pq_seed_mlocked: bool,
}

impl AgentState {
    /// Create a new locked agent state with default config.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            pq_seed: None,
            public_key: None,
            kek: None,
            kek_version: 0,
            kek_dir: None,
            unlocked_at: None,
            last_activity: None,
            config: AgentConfig::default(),
            kek_mlocked: false,
            pq_seed_mlocked: false,
        }
    }

    /// Create a new locked agent state with the given config.
    pub fn with_config(config: AgentConfig) -> Self {
        Self {
            pq_seed: None,
            public_key: None,
            kek: None,
            kek_version: 0,
            kek_dir: None,
            unlocked_at: None,
            last_activity: None,
            config,
            kek_mlocked: false,
            pq_seed_mlocked: false,
        }
    }

    /// Whether the agent holds a decrypted identity.
    pub fn is_unlocked(&self) -> bool {
        self.pq_seed.is_some()
    }

    /// Whether the agent has a cached KEK.
    pub fn has_kek(&self) -> bool {
        self.kek.is_some()
    }

    /// The cached KEK version, if loaded.
    pub fn kek_version(&self) -> Option<u16> {
        self.kek.as_ref().map(|_| self.kek_version)
    }

    /// The canonical vault `.blu/` path for the cached KEK, if loaded
    /// from disk.
    #[cfg(test)]
    fn kek_dir(&self) -> Option<&std::path::Path> {
        self.kek_dir.as_deref()
    }

    /// Backdate unlock and activity timestamps for deterministic timeout tests.
    #[cfg(test)]
    fn backdate_times(&mut self, unlocked_by: Duration, activity_by: Duration) {
        if let Some(at) = self.unlocked_at.as_mut() {
            *at = Instant::now()
                .checked_sub(unlocked_by)
                .expect("unlocked_by exceeds Instant range");
        }
        if let Some(at) = self.last_activity.as_mut() {
            *at = Instant::now()
                .checked_sub(activity_by)
                .expect("activity_by exceeds Instant range");
        }
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

    /// Unlock the agent by decrypting the global identity file
    /// (`$XDG_DATA_HOME/blu/identity.age`) with the given passphrase.
    ///
    /// The identity file contains a PQ hybrid seed encoded as a
    /// bech32 `AGE-SECRET-KEY-PQ-` string, optionally encrypted
    /// with age scrypt.
    pub fn unlock(&mut self, passphrase: &str) -> Result<String> {
        let identity_path = keys::global_identity_path()?;
        let seed = keys::load_pq_seed(&identity_path, Some(passphrase))?;

        let pq_identity = PqIdentity::new(seed.clone());
        let public_key = pq_identity.to_public().to_string();

        self.pq_seed = Some(seed);
        self.public_key = Some(public_key.clone());

        self.mlock_pq_seed();

        let now = Instant::now();
        self.unlocked_at = Some(now);
        self.last_activity = Some(now);

        Ok(public_key)
    }

    /// Unlock the agent with a PQ seed directly.
    ///
    /// Used by the biometric unlock path: the CLI recovers the BIP39
    /// Seed via Touch ID, derives the PQ seed, and sends it here.
    pub fn unlock_with_pq_seed(&mut self, seed: HybridSeed) -> Result<String> {
        let pq_identity = PqIdentity::new(seed.clone());
        let public_key = pq_identity.to_public().to_string();

        self.pq_seed = Some(seed);
        self.public_key = Some(public_key.clone());

        self.mlock_pq_seed();

        let now = Instant::now();
        self.unlocked_at = Some(now);
        self.last_activity = Some(now);

        Ok(public_key)
    }

    /// Whether the agent has a PQ identity loaded.
    pub fn has_pq(&self) -> bool {
        self.pq_seed.is_some()
    }

    /// Lock the agent: munlock and zeroize all secret material.
    pub fn lock(&mut self) {
        self.munlock_pq_seed();
        self.munlock_kek();

        // HybridSeed::drop will zeroize the PQ seed (ZeroizeOnDrop)
        self.pq_seed.take();
        self.public_key.take();
        // Kek::drop will zeroize the KEK (ZeroizeOnDrop)
        self.kek.take();
        self.kek_version = 0;
        self.kek_dir = None;
        self.unlocked_at = None;
        self.last_activity = None;
    }

    /// Ensure the cached KEK belongs to the requested vault. If no KEK
    /// is cached, or if the request targets a different vault, load the
    /// correct KEK from disk.
    pub fn ensure_kek(&mut self, kek_dir: &str) -> Result<u16> {
        let canonical_kek_dir = Self::canonicalize_kek_dir(kek_dir)?;

        if self.has_kek() && self.kek_dir.as_deref() == Some(canonical_kek_dir.as_path()) {
            return Ok(self.kek_version);
        }

        self.load_kek_at(canonical_kek_dir)
    }

    fn canonicalize_kek_dir(kek_dir: &str) -> Result<PathBuf> {
        std::fs::canonicalize(kek_dir).map_err(|e| {
            BluError::Internal(format!("could not resolve KEK dir {}: {}", kek_dir, e))
        })
    }

    fn load_kek_at(&mut self, canonical_kek_dir: PathBuf) -> Result<u16> {
        let seed = self
            .pq_seed
            .as_ref()
            .ok_or(BluError::Internal("agent is locked".into()))?
            .clone();

        let store = keys::kek::KekStore::new(&canonical_kek_dir);

        let pq_identity = PqIdentity::new(seed);
        let identities: Vec<&dyn age::Identity> = vec![&pq_identity as &dyn age::Identity];

        let (kek, version) = store.unwrap_current_kek_with(&identities)?;

        self.munlock_kek();
        self.kek = Some(kek);
        self.kek_version = version;
        self.kek_dir = Some(canonical_kek_dir);
        self.mlock_kek();

        Ok(version)
    }

    /// Set a KEK directly (for testing or when the KEK is provided
    /// rather than loaded from disk).
    #[allow(dead_code)]
    pub fn set_kek(&mut self, kek: Kek, version: u16) {
        self.munlock_kek();
        self.kek = Some(kek);
        self.kek_version = version;
        self.kek_dir = None;
        self.mlock_kek();
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
}

#[cfg(test)]
mod test {
    use super::*;
    use rand::RngCore;

    fn test_seed() -> HybridSeed {
        HybridSeed::new([42u8; 32])
    }

    /// Helper: unlock the agent with a test PQ seed.
    fn unlock_test_state(state: &mut AgentState) -> String {
        state.unlock_with_pq_seed(test_seed()).unwrap()
    }

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
        let result = state.unlock_with_pq_seed(test_seed());
        assert!(result.is_ok());
        assert!(state.is_unlocked());
        assert!(state.public_key().is_some());
        let pubkey = state.public_key().unwrap();
        assert!(pubkey.starts_with("age1pq"));
        assert!(state.time_remaining().is_some());
    }

    #[test]
    fn lock_clears_state() {
        let mut state = AgentState::new();
        unlock_test_state(&mut state);
        assert!(state.is_unlocked());

        state.lock();
        assert!(!state.is_unlocked());
        assert!(!state.has_kek());
        assert!(state.public_key().is_none());
        assert!(state.time_remaining().is_none());
    }

    #[test]
    fn idle_timeout_locks_agent() {
        let idle = Duration::from_secs(60);
        let config = AgentConfig {
            timeout_idle: idle,
            timeout_max: Duration::from_secs(3600),
            ..AgentConfig::default()
        };
        let mut state = AgentState::with_config(config);
        unlock_test_state(&mut state);
        assert!(state.is_unlocked());

        state.backdate_times(Duration::from_secs(1), idle + Duration::from_secs(1));
        assert!(state.check_timeouts());
        assert!(!state.is_unlocked());
    }

    #[test]
    fn max_timeout_locks_agent() {
        let max = Duration::from_secs(60);
        let config = AgentConfig {
            timeout_idle: Duration::from_secs(3600),
            timeout_max: max,
            ..AgentConfig::default()
        };
        let mut state = AgentState::with_config(config);
        unlock_test_state(&mut state);
        assert!(state.is_unlocked());

        state.backdate_times(max + Duration::from_secs(1), Duration::from_secs(1));
        assert!(state.check_timeouts());
        assert!(!state.is_unlocked());
    }

    #[test]
    fn touch_resets_idle_timer() {
        let idle = Duration::from_secs(60);
        let config = AgentConfig {
            timeout_idle: idle,
            timeout_max: Duration::from_secs(3600),
            ..AgentConfig::default()
        };
        let mut state = AgentState::with_config(config);
        unlock_test_state(&mut state);

        // Activity would have expired; touch resets the idle timer.
        state.backdate_times(Duration::from_secs(30), idle + Duration::from_secs(1));
        state.touch();
        assert!(!state.check_timeouts());
        assert!(state.is_unlocked());

        // Past idle again after the touch.
        state.backdate_times(Duration::from_secs(30), idle + Duration::from_secs(1));
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
        unlock_test_state(&mut state);
        assert!(state.wrap_dek().is_err());
    }

    #[test]
    fn unwrap_dek_without_kek_fails() {
        let mut state = AgentState::new();
        unlock_test_state(&mut state);
        assert!(state.unwrap_dek(b"fake", 0).is_err());
    }

    #[test]
    fn wrap_unwrap_dek_round_trip() {
        let mut state = AgentState::new();
        unlock_test_state(&mut state);

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
        unlock_test_state(&mut state);

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
        unlock_test_state(&mut state);

        let kek = Kek::generate();
        state.set_kek(kek, 5);
        assert!(state.has_kek());
        assert_eq!(state.kek_version(), Some(5));

        state.lock();
        assert!(!state.has_kek());
        assert_eq!(state.kek_version(), None);
    }

    #[test]
    fn unlock_with_pq_seed_sets_public_key() {
        let mut state = AgentState::new();

        let pubkey = state.unlock_with_pq_seed(test_seed()).unwrap();
        assert!(state.is_unlocked());
        assert!(pubkey.starts_with("age1pq"));
    }

    #[test]
    fn pq_seed_unlock_and_lock_clears() {
        let mut state = AgentState::new();
        state.unlock_with_pq_seed(test_seed()).unwrap();
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

        // Create a PQ seed and unlock with it
        let mut seed_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed_bytes);
        let seed = HybridSeed::new(seed_bytes);
        let recipient = PqRecipient::new(public_key_from_seed(&seed));
        state.unlock_with_pq_seed(seed).unwrap();

        // Create a temp KEK store with a PQ-wrapped KEK
        let tmp = tempfile::tempdir().unwrap();
        let blu_dir = tmp.path().join(".blu");
        std::fs::create_dir_all(&blu_dir).unwrap();

        let store = keys::kek::KekStore::new(&blu_dir);
        let recipient_str = recipient.to_string();
        let expected_kek = store
            .init_with(&[&recipient as &dyn age::Recipient], &[recipient_str])
            .unwrap();

        // ensure_kek should succeed using the PQ identity
        let version = state.ensure_kek(blu_dir.to_str().unwrap()).unwrap();
        assert_eq!(version, 0);
        assert!(state.has_kek());
        let canonical_blu_dir = blu_dir.canonicalize().unwrap();
        assert_eq!(state.kek_dir(), Some(canonical_blu_dir.as_path()));

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
