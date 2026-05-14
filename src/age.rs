//! Passphrase-based encryption helpers using the age crate.
//!
//! These functions protect the global identity file
//! (`~/.blu/identity.age`) with scrypt-derived keys. They are not
//! part of the data encryption path (which uses the KEK/DEK envelope
//! scheme via `DekProvider`).

use std::io::{Read, Write};

use crate::error::BluError;

/// Decrypt some bytes using a passphrase.
pub fn passphrase_decrypt(data: &[u8], passphrase: &str) -> Result<Vec<u8>, BluError> {
    let decryptor = age::Decryptor::new(data)?;
    let identity = age::scrypt::Identity::new(passphrase.to_owned().into());
    let mut decrypted = vec![];
    let mut reader = decryptor.decrypt(std::iter::once(&identity as &dyn age::Identity))?;
    let _ = reader.read_to_end(&mut decrypted);

    Ok(decrypted)
}

/// Encrypt some bytes using a passphrase.
pub fn passphrase_encrypt(data: &[u8], passphrase: &str) -> Result<Vec<u8>, BluError> {
    let encryptor = age::Encryptor::with_user_passphrase(passphrase.to_owned().into());

    let mut encrypted = vec![];
    let mut writer = encryptor.wrap_output(&mut encrypted)?;
    writer.write_all(data)?;
    writer.finish()?;

    Ok(encrypted)
}

#[cfg(test)]
mod test {
    const TEST_PASSPHRASE: &str = "correct horse battery staple";

    #[test]
    #[ignore] // slow (scrypt)
    fn passphrase_round_trip() {
        let data: [u8; 16] = [
            0xde, 0xae, 0xbe, 0xef, 0xde, 0xae, 0xbe, 0xef, 0xde, 0xae, 0xbe, 0xef, 0xbe, 0xef,
            0xbe, 0xef,
        ];
        let encrypted = super::passphrase_encrypt(&data, TEST_PASSPHRASE).unwrap();
        let decrypted = super::passphrase_decrypt(&encrypted, TEST_PASSPHRASE).unwrap();

        assert_eq!(decrypted, &data[..]);
    }
}
