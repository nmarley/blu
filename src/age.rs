//! Passphrase-based encryption helpers using the age crate.
//!
//! These functions protect the global identity file
//! (`~/.blu/identity.age`) with scrypt-derived keys. They are not
//! part of the data encryption path (which uses the KEK/DEK envelope
//! scheme via `DekProvider`).
//!
//! The scrypt work factor is pinned to at least [`MIN_SCRYPT_WORK_FACTOR`]
//! (N = 2^18 = 262,144) to ensure consistent protection regardless of
//! what the auto-calibration heuristic picks on a given machine.
//! Decryption is unaffected; the work factor is read from the file
//! header, so files encrypted with any work factor still decrypt.

use std::io::{Read, Write};

use crate::error::BluError;

/// Minimum scrypt work factor (log2 N) for new encryptions.
///
/// The age spec default is 18 (N = 262,144, roughly 1 second on
/// modern hardware). The auto-calibration in `age::scrypt::Recipient`
/// can pick lower values on slower machines; this constant ensures we
/// never go below a reasonable floor.
pub const MIN_SCRYPT_WORK_FACTOR: u8 = 18;

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
///
/// Uses `age::scrypt::Recipient` directly (instead of the convenience
/// `Encryptor::with_user_passphrase`) so we can pin the work factor
/// to [`MIN_SCRYPT_WORK_FACTOR`].
pub fn passphrase_encrypt(data: &[u8], passphrase: &str) -> Result<Vec<u8>, BluError> {
    let mut recipient = age::scrypt::Recipient::new(passphrase.to_owned().into());
    recipient.set_work_factor(MIN_SCRYPT_WORK_FACTOR);

    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))?;

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
