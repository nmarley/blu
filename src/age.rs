use std::io::{Read, Write};
use std::str::FromStr;

// TODO: Could have a more elegant separation of keys, enc-only keys, etc.
/// BlackBox is a "black-box" which encapsulates (and obscures) all encryption and decryption.
///
/// Anything that needs encryption or decryption in the project should use this.
#[derive(Clone)]
pub struct BlackBox {
    identities: Vec<age::x25519::Identity>,
}

impl std::fmt::Debug for BlackBox {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}

// TODO:
// - seed gen / recovery (24-word seed -> key only)
impl BlackBox {
    /// Create a new BlackBox with the given identities.
    pub fn new(priv_keys: &[&str]) -> BlackBox {
        let identities: Vec<age::x25519::Identity> = priv_keys
            .iter()
            .map(|x| age::x25519::Identity::from_str(x).unwrap())
            .collect();
        BlackBox { identities }
    }

    fn identities(&self) -> Vec<age::x25519::Identity> {
        self.identities.clone()
    }

    fn recipients(&self) -> Vec<age::x25519::Recipient> {
        self.identities.iter().map(|x| x.to_public()).collect()
    }

    fn new_encryptor(&self) -> Result<age::Encryptor, age::EncryptError> {
        let recipients = self.recipients();
        // Convert to references for the new API
        let recipient_refs: Vec<&dyn age::Recipient> = recipients
            .iter()
            .map(|r| r as &dyn age::Recipient)
            .collect();
        age::Encryptor::with_recipients(recipient_refs.into_iter())
    }

    /// Encrypt the given bytes using the identities associated with the BlackBox.
    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>, age::EncryptError> {
        let mut encrypted = vec![];
        let encryptor = self.new_encryptor()?;
        let mut writer = encryptor.wrap_output(&mut encrypted)?;
        writer.write_all(data)?;
        writer.finish()?;

        Ok(encrypted)
    }

    /// Decrypt the given bytes using the identities associated with the BlackBox.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut decrypted = vec![];
        let decryptor = age::Decryptor::new(data)?;
        let mut reader =
            decryptor.decrypt(self.identities().iter().map(|x| x as &dyn age::Identity))?;
        let _ = reader.read_to_end(&mut decrypted);

        Ok(decrypted)
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
        // dbg!(&encrypted);

        let decrypted = bbox.decrypt(&encrypted).unwrap();
        // dbg!(&decrypted);

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
