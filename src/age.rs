use std::io::{Read, Write};
use std::str::FromStr;

// TODO: Could have a more elegant separation of keys, enc-only keys, etc.
pub struct BlackBox {
    identities: Vec<age::x25519::Identity>,
}

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
