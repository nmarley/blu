use std::io::{Read, Write};
use std::str::FromStr;

use crate::agent::AgentClient;
use crate::keys::dek::Dek;
use crate::keys::kek::Kek;
use crate::v2format::{self, FileType};

/// KEK context for v2 envelope encryption.
///
/// Must be attached to a `BlackBox` for encrypt/decrypt to function.
/// All data uses v2 format (envelope encryption with KEK/DEK hierarchy).
#[derive(Clone)]
pub struct KekContext {
    /// The unwrapped KEK for this session.
    pub kek: Kek,
    /// Which KEK version this is (written into v2 headers).
    pub kek_version: u16,
}

/// The underlying crypto backend for a BlackBox.
enum BlackBoxInner {
    /// Crypto performed in-process using age identities.
    InProcess {
        identities: Vec<age::x25519::Identity>,
    },
    /// Crypto delegated to the agent daemon.
    Agent(AgentClient),
}

/// BlackBox is a "black-box" which encapsulates (and obscures) all encryption and decryption.
///
/// Anything that needs encryption or decryption in the project should use this.
///
/// Two modes exist:
/// - In-process: holds age identities directly and performs crypto in the
///   current process.
/// - Agent: delegates key wrapping to the agent daemon over a Unix
///   socket, so key material never leaves the daemon process.
///
/// A `KekContext` must be attached (via `with_kek` or `set_kek`) before
/// encrypting or decrypting data. All data uses v2 envelope format
/// (KEK/DEK hierarchy with ChaCha20-Poly1305).
pub struct BlackBox {
    inner: BlackBoxInner,
    kek_ctx: Option<KekContext>,
    /// Path to the vault's `.blu/` directory, passed to the agent
    /// daemon so it can lazily load the KEK on first use.
    kek_dir: Option<String>,
}

impl Clone for BlackBox {
    fn clone(&self) -> Self {
        let inner = match &self.inner {
            BlackBoxInner::InProcess { identities } => BlackBoxInner::InProcess {
                identities: identities.clone(),
            },
            BlackBoxInner::Agent(_) => {
                let client =
                    AgentClient::new().expect("failed to create agent client for BlackBox clone");
                BlackBoxInner::Agent(client)
            }
        };
        BlackBox {
            inner,
            kek_ctx: self.kek_ctx.clone(),
            kek_dir: self.kek_dir.clone(),
        }
    }
}

impl std::fmt::Debug for BlackBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let variant = match &self.inner {
            BlackBoxInner::InProcess { .. } => "InProcess",
            BlackBoxInner::Agent(_) => "Agent",
        };
        let has_kek = self.kek_ctx.is_some();
        f.debug_struct("BlackBox")
            .field("mode", &variant)
            .field("has_kek", &has_kek)
            .field("kek_dir", &self.kek_dir)
            .finish()
    }
}

impl BlackBox {
    /// Create a new in-process BlackBox with the given identities.
    pub fn new(priv_keys: &[&str]) -> BlackBox {
        let identities: Vec<age::x25519::Identity> = priv_keys
            .iter()
            .map(|x| age::x25519::Identity::from_str(x).unwrap())
            .collect();
        BlackBox {
            inner: BlackBoxInner::InProcess { identities },
            kek_ctx: None,
            kek_dir: None,
        }
    }

    /// Create a BlackBox that delegates to the agent daemon.
    ///
    /// `kek_dir` is the path to the vault's `.blu/` directory. It is
    /// sent to the daemon on the first `wrap_dek`/`unwrap_dek` call so
    /// the daemon can lazily load the KEK from disk.
    pub fn from_agent(client: AgentClient, kek_dir: Option<String>) -> BlackBox {
        BlackBox {
            inner: BlackBoxInner::Agent(client),
            kek_ctx: None,
            kek_dir,
        }
    }

    /// Attach a KEK context, enabling v2 envelope encryption.
    ///
    /// Returns self for chaining.
    pub fn with_kek(mut self, kek: Kek, kek_version: u16) -> Self {
        self.kek_ctx = Some(KekContext { kek, kek_version });
        self
    }

    /// Set the KEK context on an existing BlackBox.
    pub fn set_kek(&mut self, kek: Kek, kek_version: u16) {
        self.kek_ctx = Some(KekContext { kek, kek_version });
    }

    /// Get a reference to the KEK context, if one is set.
    pub fn kek_context(&self) -> Option<&KekContext> {
        self.kek_ctx.as_ref()
    }

    /// Whether this BlackBox has a KEK context (v2 capable).
    pub fn has_kek(&self) -> bool {
        self.kek_ctx.is_some()
    }

    /// Encrypt data as a blob file (v2 envelope format).
    ///
    /// For `InProcess`, uses the attached KEK context.
    /// For `Agent`, delegates DEK wrapping to the daemon via `wrap_dek` RPC,
    /// then encrypts data in-process with the DEK.
    pub fn encrypt_blob(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        self.encrypt_typed(data, FileType::Blob)
    }

    /// Encrypt data as an index file (v2 envelope format).
    ///
    /// For `InProcess`, uses the attached KEK context.
    /// For `Agent`, delegates DEK wrapping to the daemon via `wrap_dek` RPC,
    /// then encrypts data in-process with the DEK.
    pub fn encrypt_index(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        self.encrypt_typed(data, FileType::Index)
    }

    /// Internal: encrypt with a specific file type, dispatching to
    /// the correct path based on InProcess vs Agent.
    fn encrypt_typed(
        &self,
        data: &[u8],
        file_type: FileType,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        match &self.inner {
            BlackBoxInner::InProcess { .. } => {
                let kek_ctx = self.kek_ctx.as_ref().ok_or_else(|| {
                    crate::error::BluError::EncryptionFailed(
                        "no KEK available for encryption".into(),
                    )
                })?;
                v2format::encrypt_v2(data, &kek_ctx.kek, kek_ctx.kek_version, file_type)
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
            }
            BlackBoxInner::Agent(client) => {
                let (dek_bytes, wrapped_dek, kek_version) =
                    client.wrap_dek(self.kek_dir.as_deref())?;
                let dek = Dek::from_bytes(&dek_bytes)?;
                let encrypted_payload = dek
                    .encrypt_data(data)
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
                let mut output = Vec::new();
                v2format::write_v2(
                    &mut output,
                    file_type,
                    kek_version,
                    &wrapped_dek,
                    &encrypted_payload,
                )?;
                Ok(output)
            }
        }
    }

    /// Decrypt v2 envelope-encrypted data.
    ///
    /// For `InProcess`, uses the attached KEK context.
    /// For `Agent`, delegates DEK unwrapping to the daemon via
    /// `unwrap_dek` RPC, then decrypts in-process.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        if !v2format::is_v2(data) {
            return Err(crate::error::BluError::DecryptionFailed(
                "not a v2 envelope-encrypted file".into(),
            )
            .into());
        }

        match &self.inner {
            BlackBoxInner::InProcess { .. } => {
                let kek_ctx = self.kek_ctx.as_ref().ok_or_else(|| {
                    crate::error::BluError::DecryptionFailed(
                        "v2 file detected but no KEK available".into(),
                    )
                })?;
                let kek_clone = kek_ctx.kek.clone();
                v2format::decrypt_v2(data, |_version| Ok(kek_clone))
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
            }
            BlackBoxInner::Agent(client) => {
                let (header, payload_offset) = v2format::read_header(data)
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
                let dek_bytes = client
                    .unwrap_dek(
                        &header.wrapped_dek,
                        header.kek_version,
                        self.kek_dir.as_deref(),
                    )
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
                let dek = Dek::from_bytes(&dek_bytes)
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
                let payload = &data[payload_offset..];
                dek.decrypt_data(payload)
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
            }
        }
    }
}

/// Decrypt some bytes using a passphrase.
pub fn passphrase_decrypt(
    data: &[u8],
    passphrase: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let decryptor = age::Decryptor::new(data)?;
    let identity = age::scrypt::Identity::new(passphrase.to_owned().into());
    let mut decrypted = vec![];
    let mut reader = decryptor.decrypt(std::iter::once(&identity as &dyn age::Identity))?;
    let _ = reader.read_to_end(&mut decrypted);

    Ok(decrypted)
}

/// Encrypt some bytes using a passphrase.
pub fn passphrase_encrypt(
    data: &[u8],
    passphrase: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let encryptor = age::Encryptor::with_user_passphrase(passphrase.to_owned().into());

    let mut encrypted = vec![];
    let mut writer = encryptor.wrap_output(&mut encrypted)?;
    writer.write_all(data)?;
    writer.finish()?;

    Ok(encrypted)
}

#[cfg(test)]
#[allow(missing_docs)]
pub mod test {
    use super::BlackBox;
    use crate::keys::kek::Kek;
    use crate::v2format;

    pub(crate) const TEST_PASSPHRASE_ENIGMA: &str = "correct horse battery staple";
    pub(crate) const TEST_AGE_SECRET_KEY: &str = include_str!("../test/blu_secrets/blu.key");

    #[test]
    #[ignore] // slow
    fn sym_encrypt_decrypt() {
        let data: [u8; 16] = [
            0xde, 0xae, 0xbe, 0xef, 0xde, 0xae, 0xbe, 0xef, 0xde, 0xae, 0xbe, 0xef, 0xbe, 0xef,
            0xbe, 0xef,
        ];
        let encrypted = super::passphrase_encrypt(&data, TEST_PASSPHRASE_ENIGMA).unwrap();
        let decrypted = super::passphrase_decrypt(&encrypted, TEST_PASSPHRASE_ENIGMA).unwrap();

        assert_eq!(decrypted, &data[..]);
    }

    #[test]
    fn encrypt_blob_v2_with_kek() {
        let kek = Kek::generate();
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]).with_kek(kek.clone(), 0);
        let data = b"blob data for v2";

        let encrypted = bbox.encrypt_blob(data).unwrap();
        assert!(v2format::is_v2(&encrypted));

        let decrypted = bbox.decrypt(&encrypted).unwrap();
        assert_eq!(&decrypted, data);
    }

    #[test]
    fn encrypt_index_v2_with_kek() {
        let kek = Kek::generate();
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]).with_kek(kek.clone(), 5);
        let data = b"index data for v2";

        let encrypted = bbox.encrypt_index(data).unwrap();
        assert!(v2format::is_v2(&encrypted));

        let decrypted = bbox.decrypt(&encrypted).unwrap();
        assert_eq!(&decrypted, data);
    }

    #[test]
    fn encrypt_blob_without_kek_errors() {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let data = b"should fail without kek";

        let result = bbox.encrypt_blob(data);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_non_v2_data_errors() {
        let kek = Kek::generate();
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]).with_kek(kek, 0);

        let result = bbox.decrypt(b"not a v2 file at all");
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_v2_without_kek_errors() {
        let kek = Kek::generate();
        let bbox_with = BlackBox::new(&[TEST_AGE_SECRET_KEY]).with_kek(kek, 0);
        let bbox_without = BlackBox::new(&[TEST_AGE_SECRET_KEY]);

        let encrypted = bbox_with.encrypt_blob(b"secret").unwrap();
        let result = bbox_without.decrypt(&encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn set_kek_on_existing_bbox() {
        let kek = Kek::generate();
        let mut bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        assert!(!bbox.has_kek());

        bbox.set_kek(kek, 0);
        assert!(bbox.has_kek());

        let encrypted = bbox.encrypt_blob(b"after set_kek").unwrap();
        assert!(v2format::is_v2(&encrypted));
    }

    #[test]
    fn clone_preserves_kek() {
        let kek = Kek::generate();
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]).with_kek(kek, 3);
        let bbox2 = bbox.clone();

        assert!(bbox2.has_kek());
        assert_eq!(bbox2.kek_context().unwrap().kek_version, 3);
    }
}
