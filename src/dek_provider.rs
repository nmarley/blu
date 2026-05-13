//! Envelope key provider for wrap/unwrap of Data Encryption Keys.
//!
//! `DekProvider` is the central abstraction for key management in the
//! envelope encryption scheme. It handles only KEK/DEK wrapping, while
//! bulk data encryption is performed locally by free functions in this
//! module.
//!
//! Two variants exist:
//!
//! - `Local`: holds an unwrapped KEK in-process. Used during vault
//!   initialization (before the agent daemon is involved).
//! - `Agent`: delegates key wrapping to the agent daemon over a Unix
//!   socket. Key material never leaves the daemon process.

use crate::agent::AgentClient;
use crate::error::{BluError, Result};
use crate::keys::dek::Dek;
use crate::keys::kek::Kek;
use crate::v2format::{self, FileType};

/// Provides DEK wrapping and unwrapping using the vault's KEK.
///
/// This is the key management seam in the envelope encryption scheme.
/// All bulk data encryption happens locally with a DEK; `DekProvider`
/// controls only who holds the KEK and how DEKs are wrapped/unwrapped.
pub enum DekProvider {
    /// KEK held in the current process.
    ///
    /// Used during `blu init` (vault creation) before the agent daemon
    /// is involved. The KEK and its version are held directly.
    Local {
        /// The unwrapped KEK for this session.
        kek: Kek,
        /// Which KEK version this is (written into v2 headers).
        kek_version: u16,
    },
    /// KEK held by the agent daemon.
    ///
    /// The agent manages the KEK lifecycle (loading from disk, caching,
    /// zeroizing on lock/timeout). The client sends wrap/unwrap RPCs
    /// over a Unix socket; plaintext key material never crosses the
    /// process boundary except for ephemeral DEKs.
    Agent {
        /// Client connection to the agent daemon.
        client: AgentClient,
        /// Path to the vault's `.blu/` directory, sent to the agent so
        /// it can lazily load the correct KEK on first use.
        kek_dir: Option<String>,
    },
}

impl Clone for DekProvider {
    fn clone(&self) -> Self {
        match self {
            DekProvider::Local { kek, kek_version } => DekProvider::Local {
                kek: kek.clone(),
                kek_version: *kek_version,
            },
            DekProvider::Agent { kek_dir, .. } => {
                let client = AgentClient::new()
                    .expect("failed to create agent client for DekProvider clone");
                DekProvider::Agent {
                    client,
                    kek_dir: kek_dir.clone(),
                }
            }
        }
    }
}

impl std::fmt::Debug for DekProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DekProvider::Local { kek_version, .. } => f
                .debug_struct("DekProvider::Local")
                .field("kek_version", kek_version)
                .finish(),
            DekProvider::Agent { kek_dir, .. } => f
                .debug_struct("DekProvider::Agent")
                .field("kek_dir", kek_dir)
                .finish(),
        }
    }
}

impl DekProvider {
    /// Generate a fresh DEK and wrap it with the KEK.
    ///
    /// Returns the plaintext DEK (for encrypting data locally), the
    /// wrapped DEK bytes (for storing in the file header), and the
    /// KEK version used.
    pub fn wrap_dek(&self) -> Result<(Dek, Vec<u8>, u16)> {
        match self {
            DekProvider::Local { kek, kek_version } => {
                let dek = Dek::generate();
                let wrapped = dek.wrap(kek)?;
                Ok((dek, wrapped, *kek_version))
            }
            DekProvider::Agent { client, kek_dir } => {
                let (dek_bytes, wrapped_dek, kek_version) = client.wrap_dek(kek_dir.as_deref())?;
                let dek = Dek::from_bytes(&dek_bytes)?;
                Ok((dek, wrapped_dek, kek_version))
            }
        }
    }

    /// Unwrap a DEK from its wrapped form using the KEK.
    ///
    /// The `version` parameter is the KEK version stored in the file
    /// header. For the `Local` variant, it must match the version held
    /// by this provider; otherwise an error is returned. For the
    /// `Agent` variant, version validation is handled by the daemon.
    pub fn unwrap_dek(&self, wrapped: &[u8], version: u16) -> Result<Dek> {
        match self {
            DekProvider::Local { kek, kek_version } => {
                if version != *kek_version {
                    return Err(BluError::DecryptionFailed(format!(
                        "KEK version mismatch: file requires v{}, provider has v{}",
                        version, kek_version
                    )));
                }
                Dek::unwrap(kek, wrapped)
            }
            DekProvider::Agent { client, kek_dir } => {
                let dek_bytes = client.unwrap_dek(wrapped, version, kek_dir.as_deref())?;
                Dek::from_bytes(&dek_bytes)
            }
        }
    }
}

/// Encrypt data in v2 envelope format.
///
/// Wraps a fresh DEK with the provider's KEK, encrypts the payload
/// with ChaCha20-Poly1305, and assembles the complete file
/// (header + encrypted payload).
pub fn encrypt_envelope(data: &[u8], file_type: FileType, keys: &DekProvider) -> Result<Vec<u8>> {
    let (dek, wrapped_dek, kek_version) = keys.wrap_dek()?;
    let encrypted_payload = dek.encrypt_data(data)?;

    let mut output = Vec::new();
    v2format::write_v2(
        &mut output,
        file_type,
        kek_version,
        &wrapped_dek,
        &encrypted_payload,
    )
    .map_err(|e| BluError::EncryptionFailed(e.to_string()))?;

    Ok(output)
}

/// Decrypt v2 envelope-encrypted data.
///
/// Parses the file header, unwraps the DEK via the provider, and
/// decrypts the payload with ChaCha20-Poly1305.
pub fn decrypt_envelope(data: &[u8], keys: &DekProvider) -> Result<Vec<u8>> {
    if !v2format::is_v2(data) {
        return Err(BluError::DecryptionFailed(
            "not a v2 envelope-encrypted file".into(),
        ));
    }

    let (header, payload_offset) = v2format::read_header(data)?;
    let dek = keys.unwrap_dek(&header.wrapped_dek, header.kek_version)?;
    let payload = &data[payload_offset..];
    dek.decrypt_data(payload)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::keys::kek::Kek;

    fn local_provider(kek: &Kek, version: u16) -> DekProvider {
        DekProvider::Local {
            kek: kek.clone(),
            kek_version: version,
        }
    }

    #[test]
    fn encrypt_decrypt_blob() {
        let kek = Kek::generate();
        let keys = local_provider(&kek, 0);
        let data = b"blob data for v2";

        let encrypted = encrypt_envelope(data, FileType::Blob, &keys).unwrap();
        assert!(v2format::is_v2(&encrypted));

        let decrypted = decrypt_envelope(&encrypted, &keys).unwrap();
        assert_eq!(&decrypted, data);
    }

    #[test]
    fn encrypt_decrypt_index() {
        let kek = Kek::generate();
        let keys = local_provider(&kek, 5);
        let data = b"index data for v2";

        let encrypted = encrypt_envelope(data, FileType::Index, &keys).unwrap();
        assert!(v2format::is_v2(&encrypted));

        let decrypted = decrypt_envelope(&encrypted, &keys).unwrap();
        assert_eq!(&decrypted, data);
    }

    #[test]
    fn decrypt_non_v2_data_errors() {
        let kek = Kek::generate();
        let keys = local_provider(&kek, 0);

        let result = decrypt_envelope(b"not a v2 file at all", &keys);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_with_wrong_kek_errors() {
        let kek1 = Kek::generate();
        let kek2 = Kek::generate();
        let keys_write = local_provider(&kek1, 0);
        let keys_read = local_provider(&kek2, 0);

        let encrypted = encrypt_envelope(b"secret", FileType::Blob, &keys_write).unwrap();
        let result = decrypt_envelope(&encrypted, &keys_read);
        assert!(result.is_err());
    }

    #[test]
    fn version_mismatch_errors() {
        let kek = Kek::generate();
        let keys_v0 = local_provider(&kek, 0);
        let keys_v1 = local_provider(&kek, 1);

        let encrypted = encrypt_envelope(b"secret", FileType::Blob, &keys_v0).unwrap();
        let result = decrypt_envelope(&encrypted, &keys_v1);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("version mismatch"),
            "expected version mismatch error, got: {}",
            err_msg
        );
    }

    #[test]
    fn clone_preserves_local_state() {
        let kek = Kek::generate();
        let keys = local_provider(&kek, 3);
        let keys2 = keys.clone();

        let encrypted = encrypt_envelope(b"cloned", FileType::Blob, &keys).unwrap();
        let decrypted = decrypt_envelope(&encrypted, &keys2).unwrap();
        assert_eq!(&decrypted, b"cloned");
    }

    #[test]
    fn debug_does_not_leak_key_material() {
        let kek = Kek::generate();
        let keys = local_provider(&kek, 7);
        let debug_str = format!("{:?}", keys);
        assert!(debug_str.contains("kek_version: 7"));
        assert!(
            !debug_str.contains("kek:"),
            "debug output must not contain key material"
        );
    }
}
