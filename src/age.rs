use std::io::{Read, Write};
use std::str::FromStr;

use crate::agent::AgentClient;
use crate::keys::kek::Kek;
use crate::v2format::{self, FileType};

/// Optional KEK context for v2 envelope encryption.
///
/// When attached to a `BlackBox`, encrypt methods will produce v2
/// format files (envelope encryption with KEK/DEK hierarchy).
/// Decrypt will auto-detect v1 vs v2 regardless.
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
///   current process. This is the original behavior.
/// - Agent: delegates encrypt/decrypt to the agent daemon over a Unix
///   socket, so key material never leaves the daemon process.
///
/// An optional `KekContext` enables v2 envelope encryption. When
/// present, `encrypt_blob()` and `encrypt_index()` produce v2
/// format, and `decrypt()` auto-detects v1 vs v2.
pub struct BlackBox {
    inner: BlackBoxInner,
    kek_ctx: Option<KekContext>,
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
        }
    }

    /// Create a BlackBox that delegates to the agent daemon.
    pub fn from_agent(client: AgentClient) -> BlackBox {
        BlackBox {
            inner: BlackBoxInner::Agent(client),
            kek_ctx: None,
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

    fn identities(&self) -> Vec<age::x25519::Identity> {
        match &self.inner {
            BlackBoxInner::InProcess { identities } => identities.clone(),
            BlackBoxInner::Agent(_) => {
                panic!("cannot access identities on agent-backed BlackBox")
            }
        }
    }

    fn recipients(&self) -> Vec<age::x25519::Recipient> {
        self.identities().iter().map(|x| x.to_public()).collect()
    }

    fn new_encryptor(&self) -> Result<age::Encryptor, age::EncryptError> {
        let recipients = self.recipients();
        let recipient_refs: Vec<&dyn age::Recipient> = recipients
            .iter()
            .map(|r| r as &dyn age::Recipient)
            .collect();
        age::Encryptor::with_recipients(recipient_refs.into_iter())
    }

    /// Encrypt using age (v1 format). This always produces v1 output
    /// regardless of whether a KEK is attached.
    ///
    /// Prefer `encrypt_blob()` or `encrypt_index()` for new code.
    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        match &self.inner {
            BlackBoxInner::InProcess { .. } => {
                let mut encrypted = vec![];
                let encryptor = self.new_encryptor()?;
                let mut writer = encryptor.wrap_output(&mut encrypted)?;
                writer.write_all(data)?;
                writer.finish()?;
                Ok(encrypted)
            }
            BlackBoxInner::Agent(client) => client
                .encrypt(data)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>),
        }
    }

    /// Encrypt data as a blob file.
    ///
    /// If a KEK is attached, produces v2 format (envelope encryption).
    /// Otherwise falls back to v1 (age).
    pub fn encrypt_blob(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let kek_arg = self.kek_ctx.as_ref().map(|ctx| (&ctx.kek, ctx.kek_version));
        v2format::encrypt_auto(data, self, kek_arg, FileType::Blob)
    }

    /// Encrypt data as an index file.
    ///
    /// If a KEK is attached, produces v2 format (envelope encryption).
    /// Otherwise falls back to v1 (age).
    pub fn encrypt_index(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let kek_arg = self.kek_ctx.as_ref().map(|ctx| (&ctx.kek, ctx.kek_version));
        v2format::encrypt_auto(data, self, kek_arg, FileType::Index)
    }

    /// Decrypt data, auto-detecting v1 (age) or v2 (envelope) format.
    ///
    /// For v2 files, uses the attached KEK context to unwrap the DEK.
    /// For v1 files, uses age decryption directly.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        if v2format::is_v2(data) {
            let kek_ctx = self.kek_ctx.as_ref().ok_or_else(|| {
                crate::error::BluError::DecryptionFailed(
                    "v2 file detected but no KEK available (vault not migrated?)".into(),
                )
            })?;
            let kek_clone = kek_ctx.kek.clone();
            v2format::decrypt_v2(data, |_version| Ok(kek_clone))
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        } else {
            match &self.inner {
                BlackBoxInner::InProcess { .. } => {
                    let mut decrypted = vec![];
                    let decryptor = age::Decryptor::new(data)?;
                    let mut reader = decryptor
                        .decrypt(self.identities().iter().map(|x| x as &dyn age::Identity))?;
                    let _ = reader.read_to_end(&mut decrypted);
                    Ok(decrypted)
                }
                BlackBoxInner::Agent(client) => client
                    .decrypt(data)
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>),
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
    fn asym_encrypt_decrypt() {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let data: [u8; 5] = [0x64, 0xff, 0xcd, 0xbf, 0xbb];

        let encrypted = bbox.encrypt(&data).unwrap();

        let decrypted = bbox.decrypt(&encrypted).unwrap();

        assert_eq!(decrypted, &data[..]);
    }

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
    fn encrypt_blob_v1_without_kek() {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let data = b"blob data for v1";

        let encrypted = bbox.encrypt_blob(data).unwrap();
        assert!(!v2format::is_v2(&encrypted));

        let decrypted = bbox.decrypt(&encrypted).unwrap();
        assert_eq!(&decrypted, data);
    }

    #[test]
    fn decrypt_auto_detects_v1() {
        let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
        let data = b"v1 fallback";

        let encrypted = bbox.encrypt(data).unwrap();
        assert!(!v2format::is_v2(&encrypted));

        let decrypted = bbox.decrypt(&encrypted).unwrap();
        assert_eq!(&decrypted, data);
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
