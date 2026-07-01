//! Data Encryption Key (DEK) management.
//!
//! A DEK is a 256-bit symmetric key used to encrypt a single file
//! (blob or index). Each file gets its own DEK, generated fresh at
//! write time. The DEK is wrapped (encrypted) with the vault's KEK
//! using ChaCha20-Poly1305 and stored in the file header.
//!
//! Wrapped DEK wire format: `nonce (12 bytes) || ciphertext || tag (16 bytes)`
//!
//! The plaintext DEK is ephemeral: it exists in memory only during
//! the write or read operation, then is zeroized.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{BluError, Result};
use crate::keys::kek::Kek;

/// Size of a DEK in bytes (256 bits).
pub const DEK_SIZE: usize = 32;

/// Size of the ChaCha20-Poly1305 nonce in bytes.
const NONCE_SIZE: usize = 12;

/// Size of the ChaCha20-Poly1305 authentication tag in bytes.
const TAG_SIZE: usize = 16;

/// Overhead added by wrapping: nonce + tag.
pub const WRAP_OVERHEAD: usize = NONCE_SIZE + TAG_SIZE;

/// A plaintext DEK. Zeroized on drop.
#[derive(Clone, ZeroizeOnDrop)]
pub struct Dek {
    #[zeroize]
    bytes: [u8; DEK_SIZE],
}

impl Dek {
    /// Generate a new random DEK using the OS CSPRNG.
    pub fn generate() -> Self {
        let mut bytes = [0u8; DEK_SIZE];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Create a DEK from raw bytes. Returns an error if the length
    /// is wrong.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != DEK_SIZE {
            return Err(BluError::InvalidKeyFormat(format!(
                "DEK must be {} bytes, got {}",
                DEK_SIZE,
                data.len()
            )));
        }
        let mut bytes = [0u8; DEK_SIZE];
        bytes.copy_from_slice(data);
        Ok(Self { bytes })
    }

    /// Access the raw key bytes.
    pub fn as_bytes(&self) -> &[u8; DEK_SIZE] {
        &self.bytes
    }

    /// Wrap (encrypt) this DEK with a KEK using ChaCha20-Poly1305.
    ///
    /// Returns `nonce (12) || ciphertext (32) || tag (16)` = 60 bytes.
    pub fn wrap(&self, kek: &Kek) -> Result<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new(kek.as_bytes().into());

        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, self.bytes.as_ref())
            .map_err(|e| BluError::EncryptionFailed(format!("DEK wrap: {}", e)))?;

        // nonce || ciphertext+tag
        let mut wrapped = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        wrapped.extend_from_slice(&nonce_bytes);
        wrapped.extend_from_slice(&ciphertext);

        Ok(wrapped)
    }

    /// Unwrap (decrypt) a DEK from its wrapped form using a KEK.
    ///
    /// Expects the format produced by `wrap()`:
    /// `nonce (12) || ciphertext (32) || tag (16)`.
    pub fn unwrap(kek: &Kek, wrapped: &[u8]) -> Result<Self> {
        if wrapped.len() < NONCE_SIZE + TAG_SIZE {
            return Err(BluError::DecryptionFailed(format!(
                "wrapped DEK too short: {} bytes (minimum {})",
                wrapped.len(),
                NONCE_SIZE + TAG_SIZE
            )));
        }

        let (nonce_bytes, ciphertext_and_tag) = wrapped.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        let cipher = ChaCha20Poly1305::new(kek.as_bytes().into());
        let mut plaintext = cipher
            .decrypt(nonce, ciphertext_and_tag)
            .map_err(|_| BluError::DecryptionFailed("DEK unwrap: authentication failed".into()))?;

        let dek = Self::from_bytes(&plaintext)?;

        plaintext.zeroize();
        Ok(dek)
    }

    /// Encrypt arbitrary data with this DEK using ChaCha20-Poly1305.
    ///
    /// Returns `nonce (12) || ciphertext || tag (16)`.
    pub fn encrypt_data(&self, data: &[u8]) -> Result<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new((&self.bytes).into());

        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, data)
            .map_err(|e| BluError::EncryptionFailed(format!("DEK encrypt: {}", e)))?;

        let mut output = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&ciphertext);

        Ok(output)
    }

    /// Decrypt data that was encrypted with `encrypt_data()`.
    ///
    /// Expects `nonce (12) || ciphertext || tag (16)`.
    pub fn decrypt_data(&self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < NONCE_SIZE + TAG_SIZE {
            return Err(BluError::DecryptionFailed(format!(
                "ciphertext too short: {} bytes (minimum {})",
                data.len(),
                NONCE_SIZE + TAG_SIZE
            )));
        }

        let (nonce_bytes, ciphertext_and_tag) = data.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        let cipher = ChaCha20Poly1305::new((&self.bytes).into());
        cipher
            .decrypt(nonce, ciphertext_and_tag)
            .map_err(|_| BluError::DecryptionFailed("DEK decrypt: authentication failed".into()))
    }

    /// Encrypt a single segment of a v3 blob with this DEK.
    ///
    /// The nonce is derived deterministically from the segment index
    /// (4 zero bytes + 8-byte little-endian counter), not randomly.
    /// The segment index is also passed as AEAD associated data so a
    /// segment cannot be reordered or spliced into a different
    /// position without failing authentication.
    ///
    /// Returns `ciphertext || tag (16)` (no inline nonce; the nonce is
    /// derived from the index by the caller's reader).
    pub fn encrypt_segment(&self, index: u64, plaintext: &[u8]) -> Result<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new((&self.bytes).into());

        let nonce_bytes = segment_nonce(index);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let aad = index.to_le_bytes();
        let payload = Payload {
            msg: plaintext,
            aad: &aad,
        };

        cipher
            .encrypt(nonce, payload)
            .map_err(|e| BluError::EncryptionFailed(format!("DEK encrypt_segment: {}", e)))
    }

    /// Decrypt a single segment that was encrypted with
    /// [`encrypt_segment`](Self::encrypt_segment).
    ///
    /// The caller supplies the same segment index used during
    /// encryption so the nonce and AAD can be reconstructed. Expects
    /// `ciphertext || tag (16)` (no inline nonce).
    pub fn decrypt_segment(&self, index: u64, ciphertext: &[u8]) -> Result<Vec<u8>> {
        if ciphertext.len() < TAG_SIZE {
            return Err(BluError::DecryptionFailed(format!(
                "segment ciphertext too short: {} bytes (minimum {})",
                ciphertext.len(),
                TAG_SIZE
            )));
        }

        let cipher = ChaCha20Poly1305::new((&self.bytes).into());

        let nonce_bytes = segment_nonce(index);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let aad = index.to_le_bytes();
        let payload = Payload {
            msg: ciphertext,
            aad: &aad,
        };

        cipher.decrypt(nonce, payload).map_err(|_| {
            BluError::DecryptionFailed("DEK decrypt_segment: authentication failed".into())
        })
    }
}

/// Construct the deterministic 12-byte nonce for a v3 segment.
///
/// The nonce is `[0x00; 4] || index.to_le_bytes()`. The 4-byte zero
/// prefix reserves room for a future key-version or domain-separation
/// byte without changing the nonce length. Uniqueness is guaranteed
/// because each blob gets a fresh DEK, so the `(DEK, index)` pair is
/// never reused.
pub fn segment_nonce(index: u64) -> [u8; NONCE_SIZE] {
    let mut nonce = [0u8; NONCE_SIZE];
    nonce[4..].copy_from_slice(&index.to_le_bytes());
    nonce
}

impl std::fmt::Debug for Dek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dek").finish()
    }
}

/// Generate a new DEK, wrap it with the given KEK, and return both.
///
/// This is the operation the agent performs for the `wrap_dek` RPC:
/// the caller gets the plaintext DEK (for encrypting data in-process)
/// and the wrapped DEK (to store in the file header).
pub fn generate_and_wrap(kek: &Kek) -> Result<(Dek, Vec<u8>)> {
    let dek = Dek::generate();
    let wrapped = dek.wrap(kek)?;
    Ok((dek, wrapped))
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::keys::kek::Kek;

    #[test]
    fn generate_dek_is_random() {
        let d1 = Dek::generate();
        let d2 = Dek::generate();
        assert_ne!(d1.as_bytes(), d2.as_bytes());
    }

    #[test]
    fn dek_from_bytes_valid() {
        let bytes = [0xCDu8; DEK_SIZE];
        let dek = Dek::from_bytes(&bytes).unwrap();
        assert_eq!(dek.as_bytes(), &bytes);
    }

    #[test]
    fn dek_from_bytes_wrong_size() {
        assert!(Dek::from_bytes(&[0u8; 16]).is_err());
        assert!(Dek::from_bytes(&[0u8; 64]).is_err());
    }

    #[test]
    fn wrap_unwrap_round_trip() {
        let kek = Kek::generate();
        let dek = Dek::generate();

        let wrapped = dek.wrap(&kek).unwrap();
        assert_eq!(wrapped.len(), NONCE_SIZE + DEK_SIZE + TAG_SIZE);
        assert_ne!(&wrapped[NONCE_SIZE..], dek.as_bytes().as_slice());

        let unwrapped = Dek::unwrap(&kek, &wrapped).unwrap();
        assert_eq!(unwrapped.as_bytes(), dek.as_bytes());
    }

    #[test]
    fn unwrap_with_wrong_kek_fails() {
        let kek1 = Kek::generate();
        let kek2 = Kek::generate();
        let dek = Dek::generate();

        let wrapped = dek.wrap(&kek1).unwrap();
        let result = Dek::unwrap(&kek2, &wrapped);
        assert!(result.is_err());
    }

    #[test]
    fn unwrap_truncated_fails() {
        let kek = Kek::generate();
        let result = Dek::unwrap(&kek, &[0u8; 10]);
        assert!(result.is_err());
    }

    #[test]
    fn unwrap_tampered_fails() {
        let kek = Kek::generate();
        let dek = Dek::generate();
        let mut wrapped = dek.wrap(&kek).unwrap();

        // Flip a byte in the ciphertext
        let mid = NONCE_SIZE + DEK_SIZE / 2;
        wrapped[mid] ^= 0xFF;

        let result = Dek::unwrap(&kek, &wrapped);
        assert!(result.is_err());
    }

    #[test]
    fn encrypt_decrypt_data_round_trip() {
        let dek = Dek::generate();
        let plaintext = b"the quick brown fox jumps over the lazy dog";

        let ciphertext = dek.encrypt_data(plaintext).unwrap();
        assert_ne!(&ciphertext, plaintext.as_slice());
        assert_eq!(ciphertext.len(), NONCE_SIZE + plaintext.len() + TAG_SIZE);

        let decrypted = dek.decrypt_data(&ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn encrypt_decrypt_empty_data() {
        let dek = Dek::generate();
        let plaintext = b"";

        let ciphertext = dek.encrypt_data(plaintext).unwrap();
        assert_eq!(ciphertext.len(), NONCE_SIZE + TAG_SIZE);

        let decrypted = dek.decrypt_data(&ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let dek1 = Dek::generate();
        let dek2 = Dek::generate();

        let ciphertext = dek1.encrypt_data(b"secret").unwrap();
        let result = dek2.decrypt_data(&ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_tampered_data_fails() {
        let dek = Dek::generate();
        let mut ciphertext = dek.encrypt_data(b"secret").unwrap();

        ciphertext[NONCE_SIZE + 2] ^= 0xFF;

        let result = dek.decrypt_data(&ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_truncated_data_fails() {
        let dek = Dek::generate();
        let result = dek.decrypt_data(&[0u8; 10]);
        assert!(result.is_err());
    }

    #[test]
    fn generate_and_wrap_works() {
        let kek = Kek::generate();
        let (dek, wrapped) = generate_and_wrap(&kek).unwrap();

        let unwrapped = Dek::unwrap(&kek, &wrapped).unwrap();
        assert_eq!(unwrapped.as_bytes(), dek.as_bytes());
    }

    #[test]
    fn full_pipeline_kek_dek_data() {
        // Simulate the full write/read pipeline:
        // 1. Generate KEK, wrap DEK with it
        // 2. Encrypt data with DEK
        // 3. Unwrap DEK with KEK
        // 4. Decrypt data with DEK
        let kek = Kek::generate();

        // Write path
        let (dek, wrapped_dek) = generate_and_wrap(&kek).unwrap();
        let plaintext = b"important vault data that must be protected";
        let ciphertext = dek.encrypt_data(plaintext).unwrap();
        drop(dek); // DEK is ephemeral

        // Read path (only have KEK and the stored wrapped_dek + ciphertext)
        let recovered_dek = Dek::unwrap(&kek, &wrapped_dek).unwrap();
        let recovered = recovered_dek.decrypt_data(&ciphertext).unwrap();
        assert_eq!(&recovered, plaintext);
    }

    #[test]
    fn segment_nonce_construction() {
        let nonce = segment_nonce(0);
        assert_eq!(&nonce[0..4], &[0u8, 0, 0, 0]);
        assert_eq!(&nonce[4..8], &[0u8, 0, 0, 0]);
        assert_eq!(&nonce[8..12], &[0u8, 0, 0, 0]);

        let nonce42 = segment_nonce(42);
        assert_eq!(&nonce42[0..4], &[0u8, 0, 0, 0]);
        assert_eq!(u64::from_le_bytes(nonce42[4..12].try_into().unwrap()), 42);
    }

    #[test]
    fn encrypt_decrypt_segment_round_trip() {
        let dek = Dek::generate();
        let plaintext = b"segment payload data";

        let ciphertext = dek.encrypt_segment(0, plaintext).unwrap();
        // No inline nonce: ciphertext + tag only.
        assert_eq!(ciphertext.len(), plaintext.len() + TAG_SIZE);

        let decrypted = dek.decrypt_segment(0, &ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn encrypt_decrypt_segment_multiple_indices() {
        let dek = Dek::generate();
        let plaintext = b"same plaintext different segments";

        for index in [0u64, 1, 2, 127, 255, 1023] {
            let ciphertext = dek.encrypt_segment(index, plaintext).unwrap();
            let decrypted = dek.decrypt_segment(index, &ciphertext).unwrap();
            assert_eq!(
                &decrypted, plaintext,
                "round-trip failed for index {}",
                index
            );
        }
    }

    #[test]
    fn decrypt_segment_wrong_index_fails() {
        let dek = Dek::generate();
        let plaintext = b"segment data";

        let ciphertext = dek.encrypt_segment(5, plaintext).unwrap();

        // Decrypting with a different index should fail (nonce/AAD mismatch).
        let result = dek.decrypt_segment(6, &ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_segment_tampered_fails() {
        let dek = Dek::generate();
        let mut ciphertext = dek.encrypt_segment(0, b"segment data").unwrap();

        // Flip a byte in the ciphertext body.
        ciphertext[2] ^= 0xFF;

        let result = dek.decrypt_segment(0, &ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_segment_truncated_fails() {
        let dek = Dek::generate();
        let result = dek.decrypt_segment(0, &[0u8; 8]);
        assert!(result.is_err());
    }

    #[test]
    fn segments_same_plaintext_different_indices_produce_different_ciphertext() {
        let dek = Dek::generate();
        let plaintext = b"identical plaintext";

        let ct0 = dek.encrypt_segment(0, plaintext).unwrap();
        let ct1 = dek.encrypt_segment(1, plaintext).unwrap();
        let ct2 = dek.encrypt_segment(2, plaintext).unwrap();

        // All three must be different (different nonces => different ciphertext).
        assert_ne!(ct0, ct1);
        assert_ne!(ct1, ct2);
        assert_ne!(ct0, ct2);

        // But all decrypt back to the same plaintext with their own index.
        assert_eq!(&dek.decrypt_segment(0, &ct0).unwrap(), plaintext);
        assert_eq!(&dek.decrypt_segment(1, &ct1).unwrap(), plaintext);
        assert_eq!(&dek.decrypt_segment(2, &ct2).unwrap(), plaintext);
    }

    #[test]
    fn decrypt_segment_with_wrong_key_fails() {
        let dek1 = Dek::generate();
        let dek2 = Dek::generate();

        let ciphertext = dek1.encrypt_segment(0, b"secret segment").unwrap();
        let result = dek2.decrypt_segment(0, &ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn encrypt_decrypt_empty_segment() {
        let dek = Dek::generate();
        let plaintext = b"";

        let ciphertext = dek.encrypt_segment(0, plaintext).unwrap();
        assert_eq!(ciphertext.len(), TAG_SIZE);

        let decrypted = dek.decrypt_segment(0, &ciphertext).unwrap();
        assert!(decrypted.is_empty());
    }
}
