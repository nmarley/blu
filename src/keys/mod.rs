//! Key management for blu.
//!
//! This module handles loading, storing, and managing age encryption keys.
//! The private key lives at `~/.blu/identity.age` (optionally passphrase-
//! encrypted) and is resolved at runtime by [`global_identity_path`].

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
use std::str::FromStr;

use age::secrecy::ExposeSecret;
use age::x25519::{Identity, Recipient};

use crate::age::{passphrase_decrypt, passphrase_encrypt, BlackBox};
use crate::error::{BluError, Result};

/// Default filename for the identity (private key) file.
const IDENTITY_FILENAME: &str = "identity.age";

/// Return the canonical path to the global identity file
/// (`~/.blu/identity.age`).
pub fn global_identity_path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| BluError::Internal("could not determine home directory".to_string()))?;
    Ok(home.join(".blu").join(IDENTITY_FILENAME))
}

/// Generate a new age keypair.
///
/// Returns the identity (private key) and recipient (public key).
pub fn generate_keypair() -> (Identity, Recipient) {
    let identity = Identity::generate();
    let recipient = identity.to_public();
    (identity, recipient)
}

/// Save an identity to a file, optionally encrypted with a passphrase.
///
/// If passphrase is Some, the identity is encrypted before saving.
/// If passphrase is None, the identity is saved in plaintext (not recommended).
pub fn save_identity<P: AsRef<Path>>(
    identity: &Identity,
    path: P,
    passphrase: Option<&str>,
) -> Result<()> {
    let identity_secret = identity.to_string();
    let identity_str = identity_secret.expose_secret();
    let bytes = identity_str.as_bytes();

    let data = match passphrase {
        Some(pass) => passphrase_encrypt(bytes, pass)
            .map_err(|e| BluError::EncryptionFailed(e.to_string()))?,
        None => bytes.to_vec(),
    };

    // Create parent directories if needed
    if let Some(parent) = path.as_ref().parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, data)?;
    Ok(())
}

/// Load an identity from a file, decrypting if necessary.
///
/// If the file is encrypted, a passphrase must be provided.
pub fn load_identity<P: AsRef<Path>>(path: P, passphrase: Option<&str>) -> Result<Identity> {
    let data = fs::read(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            BluError::KeyFileNotFound {
                path: path.as_ref().to_path_buf(),
            }
        } else {
            BluError::from(e)
        }
    })?;

    // Try to detect if the file is encrypted (age files start with "age-encryption.org")
    let is_encrypted = data.starts_with(b"age-encryption.org");

    let identity_str = if is_encrypted {
        let pass = passphrase.ok_or(BluError::PassphraseRequired)?;
        let decrypted = passphrase_decrypt(&data, pass).map_err(|_| BluError::WrongPassphrase)?;
        String::from_utf8(decrypted).map_err(|e| BluError::InvalidKeyFormat(e.to_string()))?
    } else {
        String::from_utf8(data).map_err(|e| BluError::InvalidKeyFormat(e.to_string()))?
    };

    // Parse the identity string (handle the AGE-SECRET-KEY-... format)
    let identity_str = identity_str.trim();
    Identity::from_str(identity_str).map_err(|e| BluError::InvalidKeyFormat(e.to_string()))
}

/// Parse a recipient (public key) from a string.
pub fn parse_recipient(s: &str) -> Result<Recipient> {
    s.parse()
        .map_err(|_| BluError::InvalidKeyFormat(format!("invalid recipient: {}", s)))
}

/// Create a BlackBox from an identity.
pub fn blackbox_from_identity(identity: Identity) -> BlackBox {
    let identity_secret = identity.to_string();
    let identity_str = identity_secret.expose_secret();
    BlackBox::new(&[identity_str])
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

    #[test]
    fn generate_and_save_load_plaintext() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.key");

        let (identity, _recipient) = generate_keypair();
        save_identity(&identity, &path, None).unwrap();

        let loaded = load_identity(&path, None).unwrap();
        assert_eq!(
            identity.to_string().expose_secret(),
            loaded.to_string().expose_secret()
        );
    }

    #[test]
    #[ignore] // slow due to scrypt
    fn generate_and_save_load_encrypted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.key");
        let passphrase = "test-passphrase-123";

        let (identity, _recipient) = generate_keypair();
        save_identity(&identity, &path, Some(passphrase)).unwrap();

        // Should fail without passphrase
        let result = load_identity(&path, None);
        assert!(result.is_err());

        // Should fail with wrong passphrase
        let result = load_identity(&path, Some("wrong"));
        assert!(result.is_err());

        // Should succeed with correct passphrase
        let loaded = load_identity(&path, Some(passphrase)).unwrap();
        assert_eq!(
            identity.to_string().expose_secret(),
            loaded.to_string().expose_secret()
        );
    }

    #[test]
    fn parse_recipient_valid() {
        // A valid age recipient (generated for test)
        let pubkey = "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p";
        let result = parse_recipient(pubkey);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_recipient_invalid() {
        let result = parse_recipient("not-a-valid-key");
        assert!(result.is_err());
    }
}
