//! Key management for blu.
//!
//! This module handles loading, storing, and managing encryption keys.
//! The PQ hybrid seed lives at `~/.blu/identity.age` (optionally
//! passphrase-encrypted via age scrypt) and is resolved at runtime by
//! [`global_identity_path`].

/// Data Encryption Key (DEK) generation, wrapping, and data encryption.
pub mod dek;
/// HPKE Base mode for the MLKEM768-X25519 suite (RFC 9180).
pub mod hpke;
/// MLKEM768-X25519 hybrid KEM (post-quantum).
pub mod hybrid_kem;
/// Key Encryption Key (KEK) generation, wrapping, and storage.
pub mod kek;
/// BIP39 mnemonic generation and seed-to-key derivation.
pub mod mnemonic;
/// Post-quantum age recipient and identity (mlkem768x25519).
pub mod pq;
/// Post-quantum integration tests (full pipeline + Go age interop).
#[cfg(test)]
mod pq_integration_test;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::age::{passphrase_decrypt, passphrase_encrypt};
use crate::error::{BluError, Result};
use crate::keys::hybrid_kem::HybridSeed;
use crate::keys::pq::{parse_pq_identity, PqIdentity};

/// Default filename for the identity (private key) file.
const IDENTITY_FILENAME: &str = "identity.age";

/// Return the canonical path to the global identity file
/// (`~/.blu/identity.age`).
pub fn global_identity_path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| BluError::Internal("could not determine home directory".to_string()))?;
    Ok(home.join(".blu").join(IDENTITY_FILENAME))
}

/// Save a PQ hybrid seed to a file, optionally encrypted with a
/// passphrase.
///
/// The seed is bech32-encoded with the `AGE-SECRET-KEY-PQ-` HRP.
/// When a passphrase is provided, the encoded string is encrypted
/// using age's scrypt recipient before writing.
pub fn save_pq_seed<P: AsRef<Path>>(
    seed: &HybridSeed,
    path: P,
    passphrase: Option<&str>,
) -> Result<()> {
    let identity = PqIdentity::new(seed.clone());
    let encoded = identity.to_bech32();
    let bytes = encoded.as_bytes();

    let data = match passphrase {
        Some(pass) => passphrase_encrypt(bytes, pass)
            .map_err(|e| BluError::EncryptionFailed(e.to_string()))?,
        None => bytes.to_vec(),
    };

    if let Some(parent) = path.as_ref().parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, data)?;
    Ok(())
}

/// Load a PQ hybrid seed from a file, decrypting if necessary.
///
/// Expects the file to contain a bech32-encoded PQ identity string
/// (`AGE-SECRET-KEY-PQ-...`), optionally wrapped in age scrypt
/// encryption. Returns an error if the file contains a legacy
/// `AGE-SECRET-KEY-` (X25519) identity.
pub fn load_pq_seed<P: AsRef<Path>>(path: P, passphrase: Option<&str>) -> Result<HybridSeed> {
    let data = fs::read(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            BluError::KeyFileNotFound {
                path: path.as_ref().to_path_buf(),
            }
        } else {
            BluError::from(e)
        }
    })?;

    let is_encrypted = data.starts_with(b"age-encryption.org");

    let content = if is_encrypted {
        let pass = passphrase.ok_or(BluError::PassphraseRequired)?;
        let decrypted = passphrase_decrypt(&data, pass).map_err(|_| BluError::WrongPassphrase)?;
        String::from_utf8(decrypted).map_err(|e| BluError::InvalidKeyFormat(e.to_string()))?
    } else {
        String::from_utf8(data).map_err(|e| BluError::InvalidKeyFormat(e.to_string()))?
    };

    let content = content.trim();

    if content.starts_with("AGE-SECRET-KEY-1") {
        return Err(BluError::InvalidKeyFormat(
            "legacy X25519 identity detected; run 'blu identity init' to create a PQ identity"
                .into(),
        ));
    }

    let identity = parse_pq_identity(content)?;
    Ok(identity.seed().clone())
}

/// Prompt for a passphrase on stdin with hidden input.
///
/// If `confirm` is true, prompts twice and verifies they match.
pub fn prompt_passphrase(prompt: &str, confirm: bool) -> Result<String> {
    let pass1 = rpassword::prompt_password(prompt)
        .map_err(|e| BluError::Internal(format!("failed to read passphrase: {}", e)))?;

    if confirm {
        let pass2 = rpassword::prompt_password("Confirm passphrase: ")
            .map_err(|e| BluError::Internal(format!("failed to read passphrase: {}", e)))?;

        if pass1 != pass2 {
            return Err(BluError::Internal("passphrases do not match".to_string()));
        }
    }

    Ok(pass1)
}

#[cfg(test)]
mod test {
    use super::*;
    use tempfile::tempdir;

    fn test_seed() -> HybridSeed {
        HybridSeed::new([42u8; 32])
    }

    #[test]
    fn save_load_pq_seed_plaintext() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.key");

        let seed = test_seed();
        save_pq_seed(&seed, &path, None).unwrap();

        let loaded = load_pq_seed(&path, None).unwrap();
        assert_eq!(seed.as_bytes(), loaded.as_bytes());
    }

    #[test]
    #[ignore] // slow due to scrypt
    fn save_load_pq_seed_encrypted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.key");
        let passphrase = "test-passphrase-123";

        let seed = test_seed();
        save_pq_seed(&seed, &path, Some(passphrase)).unwrap();

        // Should fail without passphrase
        let result = load_pq_seed(&path, None);
        assert!(result.is_err());

        // Should fail with wrong passphrase
        let result = load_pq_seed(&path, Some("wrong"));
        assert!(result.is_err());

        // Should succeed with correct passphrase
        let loaded = load_pq_seed(&path, Some(passphrase)).unwrap();
        assert_eq!(seed.as_bytes(), loaded.as_bytes());
    }

    #[test]
    fn load_legacy_x25519_identity_errors() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.key");

        // Write a legacy AGE-SECRET-KEY format
        fs::write(&path, "AGE-SECRET-KEY-1FAKE").unwrap();

        let result = load_pq_seed(&path, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("legacy X25519"));
    }
}
