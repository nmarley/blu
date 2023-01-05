use age::secrecy::Secret;
use std::io::{Read, Write};
use std::str::FromStr;

// TODO: Could have a more elegant separation of keys, enc-only keys, etc.
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
    fn new_encryptor(&self) -> age::Encryptor {
        let identities = self.identities();

        // let pub_keys: Vec<Box<dyn age::Recipient>> = identities
        //     .into_iter()
        //     .map(|x| Box::new(x.to_public()))
        //     .collect();

        let mut pub_keys: Vec<Box<dyn age::Recipient>> = vec![];
        for x in identities.into_iter() {
            let pub_key = x.to_public();
            pub_keys.push(Box::new(pub_key));
        }

        age::Encryptor::with_recipients(pub_keys)
    }

    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>, age::EncryptError> {
        let mut encrypted = vec![];
        let mut writer = self.new_encryptor().wrap_output(&mut encrypted)?;
        writer.write_all(data)?;
        writer.finish()?;

        Ok(encrypted)
    }

    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut decrypted = vec![];
        let decryptor = match age::Decryptor::new(data).unwrap() {
            age::Decryptor::Recipients(d) => d,
            _ => unreachable!(),
        };
        let mut reader =
            decryptor.decrypt(self.identities().iter().map(|x| x as &dyn age::Identity))?;
        let _ = reader.read_to_end(&mut decrypted);

        Ok(decrypted)
    }
}

pub fn passphrase_decrypt(
    data: &[u8],
    passphrase: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let decryptor = match age::Decryptor::new(data)? {
        age::Decryptor::Passphrase(d) => d,
        _ => unreachable!(),
    };
    let mut decrypted = vec![];
    let mut reader = decryptor.decrypt(&Secret::new(passphrase.to_owned()), None)?;
    let _ = reader.read_to_end(&mut decrypted);

    Ok(decrypted)
}

pub fn passphrase_encrypt(
    data: &[u8],
    passphrase: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let encryptor = age::Encryptor::with_user_passphrase(Secret::new(passphrase.to_owned()));

    let mut encrypted = vec![];
    let mut writer = encryptor.wrap_output(&mut encrypted)?;
    writer.write_all(data)?;
    writer.finish()?;

    Ok(encrypted)
}

#[cfg(test)]
pub mod test {
    use super::BlackBox;

    pub(crate) const TEST_PASSPHRASE_ENIGMA: &str = "correct horse battery staple";
    pub(crate) const TEST_AGE_SECRET_KEY_PATH: &str = "test/blu_secrets/blu.key";
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
