//! Key Encryption Key (KEK) management.
//!
//! A KEK is a 256-bit symmetric key used to wrap per-file Data
//! Encryption Keys (DEKs). The KEK itself is wrapped (encrypted)
//! using age to all authorized users' public keys, so any authorized
//! user can unwrap it using their private key.
//!
//! On-disk layout inside a vault's `.blu/` directory:
//!
//! ```text
//! .blu/keys/
//!   kek.toml              metadata (current version, authorized users)
//!   kek_v0/
//!     wrapped.age         KEK encrypted to all authorized users via age
//!   kek_v1/               (after rotation)
//!     wrapped.age
//! ```
//!
//! The plaintext KEK is never written to disk. It is decrypted by the
//! agent and held in memory for the duration of the session.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{BluError, Result};

/// Size of a KEK in bytes (256 bits).
pub const KEK_SIZE: usize = 32;

/// A plaintext KEK. Zeroized on drop.
#[derive(Clone, ZeroizeOnDrop)]
pub struct Kek {
    #[zeroize]
    bytes: [u8; KEK_SIZE],
}

impl Kek {
    /// Generate a new random KEK using the OS CSPRNG.
    pub fn generate() -> Self {
        let mut bytes = [0u8; KEK_SIZE];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Create a KEK from raw bytes. Returns an error if the length
    /// is wrong.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != KEK_SIZE {
            return Err(BluError::InvalidKeyFormat(format!(
                "KEK must be {} bytes, got {}",
                KEK_SIZE,
                data.len()
            )));
        }
        let mut bytes = [0u8; KEK_SIZE];
        bytes.copy_from_slice(data);
        Ok(Self { bytes })
    }

    /// Access the raw key bytes.
    pub fn as_bytes(&self) -> &[u8; KEK_SIZE] {
        &self.bytes
    }

    /// Wrap this KEK for the given age recipients (public key strings).
    ///
    /// Parses each string as an X25519 recipient. For PQ recipients,
    /// use `wrap_for` instead.
    pub fn wrap_for_recipients(&self, recipients: &[&str]) -> Result<Vec<u8>> {
        let parsed: Vec<age::x25519::Recipient> = recipients
            .iter()
            .map(|r| {
                age::x25519::Recipient::from_str(r)
                    .map_err(|_| BluError::InvalidKeyFormat(format!("invalid recipient: {}", r)))
            })
            .collect::<Result<Vec<_>>>()?;

        let refs: Vec<&dyn age::Recipient> =
            parsed.iter().map(|r| r as &dyn age::Recipient).collect();
        self.wrap_for(&refs)
    }

    /// Wrap this KEK for the given age recipients (trait objects).
    ///
    /// Accepts any `age::Recipient` implementation, including PQ
    /// recipients. The KEK is encrypted as an age file that any of
    /// the recipients' corresponding identities can decrypt.
    pub fn wrap_for(&self, recipients: &[&dyn age::Recipient]) -> Result<Vec<u8>> {
        let encryptor = age::Encryptor::with_recipients(recipients.iter().copied())
            .map_err(|e| BluError::EncryptionFailed(e.to_string()))?;

        let mut encrypted = vec![];
        let mut writer = encryptor
            .wrap_output(&mut encrypted)
            .map_err(|e| BluError::EncryptionFailed(e.to_string()))?;
        writer
            .write_all(&self.bytes)
            .map_err(|e| BluError::EncryptionFailed(e.to_string()))?;
        writer
            .finish()
            .map_err(|e| BluError::EncryptionFailed(e.to_string()))?;

        Ok(encrypted)
    }

    /// Unwrap a KEK from age-encrypted ciphertext using an X25519
    /// identity string. For PQ identities, use `unwrap_with` instead.
    pub fn unwrap_with_identity(ciphertext: &[u8], identity_str: &str) -> Result<Self> {
        let identity = age::x25519::Identity::from_str(identity_str)
            .map_err(|e| BluError::InvalidKeyFormat(format!("invalid identity: {}", e)))?;

        Self::unwrap_with(ciphertext, &[&identity as &dyn age::Identity])
    }

    /// Unwrap a KEK from age-encrypted ciphertext using identity trait
    /// objects.
    ///
    /// Accepts any `age::Identity` implementation, including PQ
    /// identities. Multiple identities can be provided for backward
    /// compatibility (e.g., both PQ and X25519).
    pub fn unwrap_with(ciphertext: &[u8], identities: &[&dyn age::Identity]) -> Result<Self> {
        let decryptor = age::Decryptor::new(ciphertext)
            .map_err(|e| BluError::DecryptionFailed(e.to_string()))?;

        let mut decrypted = vec![];
        let mut reader = decryptor
            .decrypt(identities.iter().copied())
            .map_err(|e| BluError::DecryptionFailed(e.to_string()))?;
        reader
            .read_to_end(&mut decrypted)
            .map_err(|e| BluError::DecryptionFailed(e.to_string()))?;

        let kek = Self::from_bytes(&decrypted)?;

        // Zeroize the intermediate buffer
        decrypted.zeroize();

        Ok(kek)
    }
}

impl std::fmt::Debug for Kek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kek").finish()
    }
}

/// Status of a KEK version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KekStatus {
    /// Current KEK, used for new encryptions.
    Active,
    /// Old KEK, kept for reading old data only.
    Deprecated,
    /// All data migrated away, can be deleted.
    Archived,
}

impl std::fmt::Display for KekStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KekStatus::Active => write!(f, "active"),
            KekStatus::Deprecated => write!(f, "deprecated"),
            KekStatus::Archived => write!(f, "archived"),
        }
    }
}

/// Metadata for a single KEK version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KekVersionInfo {
    /// The version number (0, 1, 2, ...).
    pub version: u16,
    /// When this version was created.
    pub created: String,
    /// Current status.
    pub status: KekStatus,
    /// Public keys of authorized users.
    pub users: Vec<String>,
}

/// Top-level KEK metadata stored in `kek.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KekMetadata {
    /// The current (active) version number.
    pub current_version: u16,
    /// When the KEK store was first created.
    pub created: String,
    /// Per-version metadata.
    pub versions: Vec<KekVersionInfo>,
}

impl KekMetadata {
    /// Get the info for a specific version.
    pub fn get_version(&self, version: u16) -> Option<&KekVersionInfo> {
        self.versions.iter().find(|v| v.version == version)
    }

    /// Get the active version info.
    pub fn active_version(&self) -> Option<&KekVersionInfo> {
        self.get_version(self.current_version)
    }
}

/// Manages the on-disk KEK store for a single vault.
///
/// The store lives under `.blu/keys/` within the vault directory.
pub struct KekStore {
    /// Path to `.blu/keys/`
    keys_dir: PathBuf,
}

impl KekStore {
    /// Create a KekStore for the given vault's `.blu/` directory.
    pub fn new(blu_dir: &Path) -> Self {
        Self {
            keys_dir: blu_dir.join("keys"),
        }
    }

    /// Whether a KEK store exists for this vault.
    pub fn exists(&self) -> bool {
        self.metadata_path().exists()
    }

    /// Initialize the KEK store with X25519 recipient strings.
    ///
    /// For PQ recipients, use `init_with` instead.
    pub fn init(&self, recipients: &[&str]) -> Result<Kek> {
        if self.exists() {
            return Err(BluError::Internal(
                "KEK store already exists for this vault".into(),
            ));
        }

        let kek = Kek::generate();
        let wrapped = kek.wrap_for_recipients(recipients)?;

        let version: u16 = 0;
        let now = Utc::now().to_rfc3339();

        let metadata = KekMetadata {
            current_version: version,
            created: now.clone(),
            versions: vec![KekVersionInfo {
                version,
                created: now,
                status: KekStatus::Active,
                users: recipients.iter().map(|s| s.to_string()).collect(),
            }],
        };

        self.write_metadata(&metadata)?;
        self.write_wrapped_kek(version, &wrapped)?;

        Ok(kek)
    }

    /// Initialize the KEK store with recipient trait objects.
    ///
    /// Accepts any `age::Recipient` implementations, including PQ
    /// recipients. The `user_strings` parameter stores the recipient
    /// identifiers in `kek.toml` metadata.
    pub fn init_with(
        &self,
        recipients: &[&dyn age::Recipient],
        user_strings: &[String],
    ) -> Result<Kek> {
        if self.exists() {
            return Err(BluError::Internal(
                "KEK store already exists for this vault".into(),
            ));
        }

        let kek = Kek::generate();
        let wrapped = kek.wrap_for(recipients)?;

        let version: u16 = 0;
        let now = Utc::now().to_rfc3339();

        let metadata = KekMetadata {
            current_version: version,
            created: now.clone(),
            versions: vec![KekVersionInfo {
                version,
                created: now,
                status: KekStatus::Active,
                users: user_strings.to_vec(),
            }],
        };

        self.write_metadata(&metadata)?;
        self.write_wrapped_kek(version, &wrapped)?;

        Ok(kek)
    }

    /// Load the KEK metadata from `kek.toml`.
    pub fn load_metadata(&self) -> Result<KekMetadata> {
        let path = self.metadata_path();
        let content = fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BluError::Internal("KEK store not initialized (no kek.toml)".into())
            } else {
                BluError::from(e)
            }
        })?;
        let metadata: KekMetadata = toml::from_str(&content)
            .map_err(|e| BluError::InvalidConfig(format!("kek.toml: {}", e)))?;
        Ok(metadata)
    }

    /// Read the wrapped (age-encrypted) KEK for a given version.
    pub fn read_wrapped_kek(&self, version: u16) -> Result<Vec<u8>> {
        let path = self.wrapped_kek_path(version);
        fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BluError::KeyFileNotFound { path }
            } else {
                BluError::from(e)
            }
        })
    }

    /// Unwrap the KEK for a given version using an X25519 identity string.
    pub fn unwrap_kek(&self, version: u16, identity_str: &str) -> Result<Kek> {
        let wrapped = self.read_wrapped_kek(version)?;
        Kek::unwrap_with_identity(&wrapped, identity_str)
    }

    /// Unwrap the KEK for a given version using identity trait objects.
    ///
    /// Accepts any `age::Identity` implementations, including PQ
    /// identities. Provide multiple identities for backward compat
    /// (e.g., both PQ and X25519 to decrypt old or new wrapped KEKs).
    pub fn unwrap_kek_with(&self, version: u16, identities: &[&dyn age::Identity]) -> Result<Kek> {
        let wrapped = self.read_wrapped_kek(version)?;
        Kek::unwrap_with(&wrapped, identities)
    }

    /// Unwrap the current (active) KEK using an X25519 identity string.
    pub fn unwrap_current_kek(&self, identity_str: &str) -> Result<(Kek, u16)> {
        let metadata = self.load_metadata()?;
        let version = metadata.current_version;
        let kek = self.unwrap_kek(version, identity_str)?;
        Ok((kek, version))
    }

    /// Unwrap the current (active) KEK using identity trait objects.
    pub fn unwrap_current_kek_with(&self, identities: &[&dyn age::Identity]) -> Result<(Kek, u16)> {
        let metadata = self.load_metadata()?;
        let version = metadata.current_version;
        let kek = self.unwrap_kek_with(version, identities)?;
        Ok((kek, version))
    }

    /// Add a new KEK version (for rotation). Generates a new KEK,
    /// wraps it for the given recipients, marks the old version as
    /// deprecated, and returns the new plaintext KEK.
    pub fn rotate(&self, recipients: &[&str]) -> Result<(Kek, u16)> {
        let mut metadata = self.load_metadata()?;

        let new_version = metadata.current_version + 1;
        let now = Utc::now().to_rfc3339();

        // Deprecate the old active version
        for v in &mut metadata.versions {
            if v.status == KekStatus::Active {
                v.status = KekStatus::Deprecated;
            }
        }

        let kek = Kek::generate();
        let wrapped = kek.wrap_for_recipients(recipients)?;

        metadata.versions.push(KekVersionInfo {
            version: new_version,
            created: now,
            status: KekStatus::Active,
            users: recipients.iter().map(|s| s.to_string()).collect(),
        });
        metadata.current_version = new_version;

        self.write_metadata(&metadata)?;
        self.write_wrapped_kek(new_version, &wrapped)?;

        Ok((kek, new_version))
    }

    fn metadata_path(&self) -> PathBuf {
        self.keys_dir.join("kek.toml")
    }

    fn wrapped_kek_path(&self, version: u16) -> PathBuf {
        self.keys_dir
            .join(format!("kek_v{}", version))
            .join("wrapped.age")
    }

    fn write_metadata(&self, metadata: &KekMetadata) -> Result<()> {
        fs::create_dir_all(&self.keys_dir)?;
        let toml_str = toml::to_string_pretty(metadata)
            .map_err(|e| BluError::SerializationError(e.to_string()))?;
        fs::write(self.metadata_path(), toml_str)?;
        Ok(())
    }

    fn write_wrapped_kek(&self, version: u16, wrapped: &[u8]) -> Result<()> {
        let dir = self.keys_dir.join(format!("kek_v{}", version));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("wrapped.age"), wrapped)?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use age::secrecy::ExposeSecret;
    use tempfile::tempdir;

    fn test_identity() -> (age::x25519::Identity, String) {
        let identity_str = include_str!("../../test/blu_secrets/blu.key").trim();
        let identity = age::x25519::Identity::from_str(identity_str).unwrap();
        let recipient = identity.to_public().to_string();
        (identity, recipient)
    }

    #[test]
    fn generate_kek_is_random() {
        let k1 = Kek::generate();
        let k2 = Kek::generate();
        assert_ne!(k1.as_bytes(), k2.as_bytes());
    }

    #[test]
    fn kek_from_bytes_valid() {
        let bytes = [0xABu8; KEK_SIZE];
        let kek = Kek::from_bytes(&bytes).unwrap();
        assert_eq!(kek.as_bytes(), &bytes);
    }

    #[test]
    fn kek_from_bytes_wrong_size() {
        assert!(Kek::from_bytes(&[0u8; 16]).is_err());
        assert!(Kek::from_bytes(&[0u8; 64]).is_err());
    }

    #[test]
    fn wrap_unwrap_round_trip() {
        let (identity, recipient) = test_identity();

        let kek = Kek::generate();
        let wrapped = kek.wrap_for_recipients(&[&recipient]).unwrap();
        assert_ne!(&wrapped, kek.as_bytes().as_slice());

        let identity_secret = identity.to_string();
        let identity_str = identity_secret.expose_secret();
        let unwrapped = Kek::unwrap_with_identity(&wrapped, identity_str).unwrap();
        assert_eq!(unwrapped.as_bytes(), kek.as_bytes());
    }

    #[test]
    fn wrap_for_invalid_recipient_fails() {
        let kek = Kek::generate();
        let result = kek.wrap_for_recipients(&["not-a-valid-key"]);
        assert!(result.is_err());
    }

    #[test]
    fn store_init_and_unwrap() {
        let tmp = tempdir().unwrap();
        let blu_dir = tmp.path().join(".blu");
        fs::create_dir_all(&blu_dir).unwrap();

        let (identity, recipient) = test_identity();
        let store = KekStore::new(&blu_dir);

        assert!(!store.exists());

        let kek = store.init(&[&recipient]).unwrap();

        assert!(store.exists());

        // Read back metadata
        let metadata = store.load_metadata().unwrap();
        assert_eq!(metadata.current_version, 0);
        assert_eq!(metadata.versions.len(), 1);
        assert_eq!(metadata.versions[0].status, KekStatus::Active);
        assert_eq!(metadata.versions[0].users, vec![recipient.clone()]);

        // Unwrap the KEK
        let identity_secret = identity.to_string();
        let identity_str = identity_secret.expose_secret();
        let (unwrapped, version) = store.unwrap_current_kek(identity_str).unwrap();
        assert_eq!(version, 0);
        assert_eq!(unwrapped.as_bytes(), kek.as_bytes());
    }

    #[test]
    fn store_init_twice_fails() {
        let tmp = tempdir().unwrap();
        let blu_dir = tmp.path().join(".blu");
        fs::create_dir_all(&blu_dir).unwrap();

        let (_identity, recipient) = test_identity();
        let store = KekStore::new(&blu_dir);
        store.init(&[&recipient]).unwrap();

        let result = store.init(&[&recipient]);
        assert!(result.is_err());
    }

    #[test]
    fn store_rotate() {
        let tmp = tempdir().unwrap();
        let blu_dir = tmp.path().join(".blu");
        fs::create_dir_all(&blu_dir).unwrap();

        let (identity, recipient) = test_identity();
        let store = KekStore::new(&blu_dir);

        let kek_v0 = store.init(&[&recipient]).unwrap();

        let (kek_v1, new_version) = store.rotate(&[&recipient]).unwrap();
        assert_eq!(new_version, 1);
        assert_ne!(kek_v0.as_bytes(), kek_v1.as_bytes());

        // Metadata should reflect the rotation
        let metadata = store.load_metadata().unwrap();
        assert_eq!(metadata.current_version, 1);
        assert_eq!(metadata.versions.len(), 2);
        assert_eq!(metadata.versions[0].status, KekStatus::Deprecated);
        assert_eq!(metadata.versions[1].status, KekStatus::Active);

        // Both versions should be unwrappable
        let identity_secret = identity.to_string();
        let identity_str = identity_secret.expose_secret();

        let unwrapped_v0 = store.unwrap_kek(0, identity_str).unwrap();
        assert_eq!(unwrapped_v0.as_bytes(), kek_v0.as_bytes());

        let unwrapped_v1 = store.unwrap_kek(1, identity_str).unwrap();
        assert_eq!(unwrapped_v1.as_bytes(), kek_v1.as_bytes());

        // Current should be v1
        let (current, ver) = store.unwrap_current_kek(identity_str).unwrap();
        assert_eq!(ver, 1);
        assert_eq!(current.as_bytes(), kek_v1.as_bytes());
    }

    #[test]
    fn metadata_serialization_round_trip() {
        let metadata = KekMetadata {
            current_version: 1,
            created: "2026-03-07T12:00:00Z".to_string(),
            versions: vec![
                KekVersionInfo {
                    version: 0,
                    created: "2026-03-07T12:00:00Z".to_string(),
                    status: KekStatus::Deprecated,
                    users: vec!["age1alice".to_string()],
                },
                KekVersionInfo {
                    version: 1,
                    created: "2026-03-07T13:00:00Z".to_string(),
                    status: KekStatus::Active,
                    users: vec!["age1alice".to_string(), "age1bob".to_string()],
                },
            ],
        };

        let toml_str = toml::to_string_pretty(&metadata).unwrap();
        let parsed: KekMetadata = toml::from_str(&toml_str).unwrap();

        assert_eq!(parsed.current_version, 1);
        assert_eq!(parsed.versions.len(), 2);
        assert_eq!(parsed.versions[0].status, KekStatus::Deprecated);
        assert_eq!(parsed.versions[1].status, KekStatus::Active);
        assert_eq!(parsed.versions[1].users.len(), 2);
    }

    #[test]
    fn unwrap_nonexistent_version_fails() {
        let tmp = tempdir().unwrap();
        let blu_dir = tmp.path().join(".blu");
        fs::create_dir_all(&blu_dir).unwrap();

        let (identity, recipient) = test_identity();
        let store = KekStore::new(&blu_dir);
        store.init(&[&recipient]).unwrap();

        let identity_secret = identity.to_string();
        let identity_str = identity_secret.expose_secret();
        let result = store.unwrap_kek(99, identity_str);
        assert!(result.is_err());
    }

    #[test]
    fn pq_wrap_unwrap_round_trip() {
        use crate::keys::hybrid_kem::{public_key_from_seed, HybridSeed};
        use crate::keys::pq::{PqIdentity, PqRecipient};
        use rand::RngCore;

        let mut seed_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed_bytes);
        let seed = HybridSeed::new(seed_bytes);

        let recipient = PqRecipient::new(public_key_from_seed(&seed));
        let identity = PqIdentity::new(seed);

        let kek = Kek::generate();
        let wrapped = kek.wrap_for(&[&recipient as &dyn age::Recipient]).unwrap();

        let unwrapped =
            Kek::unwrap_with(&wrapped, &[&identity as &dyn age::Identity]).unwrap();
        assert_eq!(unwrapped.as_bytes(), kek.as_bytes());
    }

    #[test]
    fn pq_store_init_and_unwrap() {
        use crate::keys::hybrid_kem::{public_key_from_seed, HybridSeed};
        use crate::keys::pq::{PqIdentity, PqRecipient};
        use rand::RngCore;

        let tmp = tempdir().unwrap();
        let blu_dir = tmp.path().join(".blu");
        fs::create_dir_all(&blu_dir).unwrap();

        let mut seed_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed_bytes);
        let seed = HybridSeed::new(seed_bytes);

        let recipient = PqRecipient::new(public_key_from_seed(&seed));
        let identity = PqIdentity::new(seed);

        let store = KekStore::new(&blu_dir);
        let recipient_str = recipient.to_string();

        // Init with PQ recipient via trait objects
        let kek = store
            .init_with(
                &[&recipient as &dyn age::Recipient],
                &[recipient_str],
            )
            .unwrap();

        // Unwrap with PQ identity via trait objects
        let (unwrapped, version) = store
            .unwrap_current_kek_with(&[&identity as &dyn age::Identity])
            .unwrap();
        assert_eq!(version, 0);
        assert_eq!(unwrapped.as_bytes(), kek.as_bytes());
    }

    #[test]
    fn pq_wrong_identity_fails() {
        use crate::keys::hybrid_kem::{public_key_from_seed, HybridSeed};
        use crate::keys::pq::{PqIdentity, PqRecipient};
        use rand::RngCore;

        let mut s1 = [0u8; 32];
        let mut s2 = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut s1);
        rand::rngs::OsRng.fill_bytes(&mut s2);

        let recipient1 = PqRecipient::new(public_key_from_seed(&HybridSeed::new(s1)));
        let identity2 = PqIdentity::new(HybridSeed::new(s2));

        let kek = Kek::generate();
        let wrapped = kek.wrap_for(&[&recipient1 as &dyn age::Recipient]).unwrap();

        let result =
            Kek::unwrap_with(&wrapped, &[&identity2 as &dyn age::Identity]);
        assert!(result.is_err());
    }
}
