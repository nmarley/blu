use std::io::{Read, Write};
use std::str::FromStr;

use crate::agent::AgentClient;

/// BlackBox is a "black-box" which encapsulates (and obscures) all encryption and decryption.
///
/// Anything that needs encryption or decryption in the project should use this.
///
/// Two variants exist:
/// - `InProcess`: holds age identities directly and performs crypto in the
///   current process. This is the original behavior.
/// - `Agent`: delegates encrypt/decrypt to the agent daemon over a Unix
///   socket, so key material never leaves the daemon process.
pub enum BlackBox {
    /// Crypto performed in-process using age identities.
    InProcess {
        /// The age x25519 identities (private keys).
        identities: Vec<age::x25519::Identity>,
    },
    /// Crypto delegated to the agent daemon.
    Agent(AgentClient),
}

impl Clone for BlackBox {
    fn clone(&self) -> Self {
        match self {
            BlackBox::InProcess { identities } => BlackBox::InProcess {
                identities: identities.clone(),
            },
            BlackBox::Agent(_) => {
                let client =
                    AgentClient::new().expect("failed to create agent client for BlackBox clone");
                BlackBox::Agent(client)
            }
        }
    }
}

impl std::fmt::Debug for BlackBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlackBox::InProcess { .. } => f.debug_struct("BlackBox::InProcess").finish(),
            BlackBox::Agent(_) => f.debug_struct("BlackBox::Agent").finish(),
        }
    }
}

impl BlackBox {
    /// Create a new in-process BlackBox with the given identities.
    pub fn new(priv_keys: &[&str]) -> BlackBox {
        let identities: Vec<age::x25519::Identity> = priv_keys
            .iter()
            .map(|x| age::x25519::Identity::from_str(x).unwrap())
            .collect();
        BlackBox::InProcess { identities }
    }

    /// Create a BlackBox that delegates to the agent daemon.
    pub fn from_agent(client: AgentClient) -> BlackBox {
        BlackBox::Agent(client)
    }

    fn identities(&self) -> Vec<age::x25519::Identity> {
        match self {
            BlackBox::InProcess { identities } => identities.clone(),
            BlackBox::Agent(_) => {
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

    /// Encrypt the given bytes.
    ///
    /// For `InProcess`, uses the age identities directly.
    /// For `Agent`, delegates to the daemon over the socket.
    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        match self {
            BlackBox::InProcess { .. } => {
                let mut encrypted = vec![];
                let encryptor = self.new_encryptor()?;
                let mut writer = encryptor.wrap_output(&mut encrypted)?;
                writer.write_all(data)?;
                writer.finish()?;
                Ok(encrypted)
            }
            BlackBox::Agent(client) => client
                .encrypt(data)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>),
        }
    }

    /// Decrypt the given bytes.
    ///
    /// For `InProcess`, uses the age identities directly.
    /// For `Agent`, delegates to the daemon over the socket.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        match self {
            BlackBox::InProcess { .. } => {
                let mut decrypted = vec![];
                let decryptor = age::Decryptor::new(data)?;
                let mut reader =
                    decryptor.decrypt(self.identities().iter().map(|x| x as &dyn age::Identity))?;
                let _ = reader.read_to_end(&mut decrypted);
                Ok(decrypted)
            }
            BlackBox::Agent(client) => client
                .decrypt(data)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>),
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
}
