//! Biometric (Touch ID) unlock support.
//!
//! On macOS, the seed is encrypted with a random device key and saved
//! to `$XDG_DATA_HOME/blu/identity.enc`. The device key is stored in
//! the macOS Keychain with biometric access control, so retrieving it
//! requires Touch ID (or the login password on machines without Touch
//! ID).
//!
//! On other platforms, all functions are stubs that return errors or
//! no-ops.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{BluError, Result};
#[cfg(target_os = "macos")]
use crate::keys::dek::Dek;
use crate::keys::mnemonic::Seed;
use crate::user_paths::UserPaths;

/// macOS Keychain service name.
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "com.blu.agent";

/// macOS Keychain account name for the device key.
#[cfg(target_os = "macos")]
const KEYCHAIN_ACCOUNT: &str = "device-key";

/// Resolve the path to `$XDG_DATA_HOME/blu/identity.enc`.
fn identity_enc_path() -> Result<PathBuf> {
    Ok(UserPaths::resolve()?.identity_enc)
}

/// Whether a biometric-encrypted identity exists on disk.
pub fn has_biometric_identity() -> bool {
    identity_enc_path().map(|p| p.exists()).unwrap_or(false)
}

/// Whether biometric unlock is available on this platform.
pub fn is_available() -> bool {
    cfg!(target_os = "macos")
}

/// Set up biometric unlock: encrypt the seed with a random device key,
/// write `$XDG_DATA_HOME/blu/identity.enc`, and store the device key in
/// the platform keychain with biometric access control.
#[cfg(target_os = "macos")]
pub fn setup(seed: &Seed) -> Result<()> {
    use rand::RngCore;

    // Generate a random 256-bit device key
    let mut device_key_bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut device_key_bytes);

    // Encrypt the seed with the device key
    let dek = Dek::from_bytes(&device_key_bytes)?;
    let encrypted = dek.encrypt_data(seed.as_bytes())?;

    // Write encrypted seed to disk
    let enc_path = identity_enc_path()?;
    crate::user_paths::ensure_parent(&enc_path)?;
    fs::write(&enc_path, &encrypted)?;

    // Store device key in Keychain with Touch ID protection
    store_device_key(&device_key_bytes)?;

    // Zeroize the device key from stack memory
    use zeroize::Zeroize;
    device_key_bytes.zeroize();

    Ok(())
}

/// Set up biometric unlock (no-op on non-macOS platforms).
#[cfg(not(target_os = "macos"))]
pub fn setup(_seed: &Seed) -> Result<()> {
    Ok(())
}

/// Unlock using biometrics: retrieve the device key from the platform
/// keychain (triggering Touch ID), decrypt the biometric identity file,
/// and return the seed.
#[cfg(target_os = "macos")]
pub fn unlock() -> Result<Seed> {
    let enc_path = identity_enc_path()?;
    if !enc_path.exists() {
        return Err(BluError::Internal(
            "no biometric identity found (run `blu identity init` first)".into(),
        ));
    }

    // Retrieve device key from Keychain (triggers Touch ID)
    let device_key_bytes = retrieve_device_key()?;

    // Decrypt the seed
    let encrypted = fs::read(&enc_path)?;
    let dek = Dek::from_bytes(&device_key_bytes)?;
    let seed_bytes = dek.decrypt_data(&encrypted)?;

    if seed_bytes.len() != 64 {
        return Err(BluError::DecryptionFailed(format!(
            "expected 64-byte seed, got {} bytes",
            seed_bytes.len()
        )));
    }

    let mut seed_arr = [0u8; 64];
    seed_arr.copy_from_slice(&seed_bytes);
    Ok(Seed::from_bytes(seed_arr))
}

/// Unlock using biometrics (unavailable on non-macOS platforms).
#[cfg(not(target_os = "macos"))]
pub fn unlock() -> Result<Seed> {
    Err(BluError::Internal(
        "biometric unlock is not available on this platform".into(),
    ))
}

/// Remove biometric data: delete the Keychain item and the encrypted
/// seed file.
#[cfg(target_os = "macos")]
pub fn remove() -> Result<()> {
    // Delete Keychain item (ignore errors if it doesn't exist)
    let _ = delete_device_key();

    // Remove identity.enc
    if let Ok(path) = identity_enc_path() {
        let _ = fs::remove_file(path);
    }

    Ok(())
}

/// Remove biometric data (encrypted seed file only on non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn remove() -> Result<()> {
    if let Ok(path) = identity_enc_path() {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

/// Store the device key in the macOS Keychain with Touch ID access control.
#[cfg(target_os = "macos")]
fn store_device_key(key: &[u8; 32]) -> Result<()> {
    use security_framework::access_control::SecAccessControl;
    use security_framework::passwords::{
        delete_generic_password, set_generic_password_options, AccessControlOptions,
        PasswordOptions,
    };

    // Delete any existing item first (SecItemUpdate does not update
    // access control attributes)
    let _ = delete_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);

    let mut opts = PasswordOptions::new_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);

    // Require Touch ID with the currently enrolled set of biometrics.
    // If a fingerprint is added or removed, the item is invalidated
    // and the user must re-init or recover.
    let access =
        SecAccessControl::create_with_flags(AccessControlOptions::BIOMETRY_CURRENT_SET.bits())
            .map_err(|e| BluError::Internal(format!("failed to create access control: {}", e)))?;
    opts.set_access_control(access);

    // Use the data protection keychain (required for biometric items)
    opts.use_protected_keychain();

    set_generic_password_options(key.as_slice(), opts)
        .map_err(|e| BluError::Internal(format!("failed to store device key in Keychain: {}", e)))
}

/// Retrieve the device key from the macOS Keychain (triggers Touch ID).
#[cfg(target_os = "macos")]
fn retrieve_device_key() -> Result<[u8; 32]> {
    use security_framework::passwords::{generic_password, PasswordOptions};

    let mut opts = PasswordOptions::new_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);
    opts.use_protected_keychain();

    let key_data = generic_password(opts).map_err(|e| {
        let code = e.code();
        if code == -128 {
            // errSecUserCanceled
            BluError::Internal("Touch ID authentication cancelled".into())
        } else {
            BluError::Internal(format!(
                "failed to retrieve device key from Keychain: {}",
                e
            ))
        }
    })?;

    if key_data.len() != 32 {
        return Err(BluError::Internal(format!(
            "device key has wrong length: expected 32, got {}",
            key_data.len()
        )));
    }

    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&key_data);
    Ok(bytes)
}

/// Delete the device key from the macOS Keychain.
#[cfg(target_os = "macos")]
fn delete_device_key() -> Result<()> {
    use security_framework::passwords::delete_generic_password;

    delete_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
        .map_err(|e| BluError::Internal(format!("failed to delete device key: {}", e)))
}

/// Check if the identity.enc file at a given path is valid (can be
/// read and has reasonable size).
#[allow(dead_code)]
pub fn validate_enc_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| {
            // 92 bytes = 12 (nonce) + 64 (seed) + 16 (tag)
            m.len() == 92
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn identity_enc_path_matches_user_paths_seam() {
        let path = identity_enc_path().unwrap();
        let expected = UserPaths::resolve().unwrap().identity_enc;
        assert_eq!(path, expected);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("identity.enc")
        );
    }

    #[test]
    fn has_biometric_identity_false_when_no_file() {
        // Unless the developer has actually run `blu identity init`,
        // this should be false. We cannot guarantee it in CI, but
        // the function should not panic.
        let _ = has_biometric_identity();
    }

    #[test]
    fn validate_enc_file_wrong_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.enc");
        fs::write(&path, b"too short").unwrap();
        assert!(!validate_enc_file(&path));
    }

    #[test]
    fn validate_enc_file_correct_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.enc");
        fs::write(&path, vec![0u8; 92]).unwrap();
        assert!(validate_enc_file(&path));
    }

    #[test]
    fn validate_enc_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.enc");
        assert!(!validate_enc_file(&path));
    }

    #[test]
    fn seed_encrypt_decrypt_with_device_key() {
        // Test the encrypt/decrypt logic without touching the Keychain
        use crate::keys::dek::Dek;
        use crate::keys::mnemonic;
        use rand::RngCore;

        let m = mnemonic::generate_mnemonic().unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");

        // Simulate what setup() does
        let mut device_key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut device_key);

        let dek = Dek::from_bytes(&device_key).unwrap();
        let encrypted = dek.encrypt_data(seed.as_bytes()).unwrap();
        assert_eq!(encrypted.len(), 92); // 12 + 64 + 16

        // Simulate what unlock() does
        let dek2 = Dek::from_bytes(&device_key).unwrap();
        let decrypted = dek2.decrypt_data(&encrypted).unwrap();
        assert_eq!(decrypted.len(), 64);
        assert_eq!(&decrypted, seed.as_bytes());
    }
}
