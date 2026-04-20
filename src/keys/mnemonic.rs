//! BIP39 mnemonic generation and seed-to-key derivation.
//!
//! A 24-word BIP39 mnemonic is the root of a user's identity. From
//! the mnemonic (plus an optional passphrase, the "25th word"), a
//! 512-bit seed is derived via PBKDF2-HMAC-SHA512 (2048 rounds).
//!
//! From the seed, algorithm-specific keys are derived using HKDF-SHA256
//! with distinct salts:
//!
//! ```text
//! seed (512 bits)
//!   |
//!   +--> HKDF-SHA256(salt="blu-x25519-v1", info="") -> 32 bytes -> age x25519 identity
//!   |
//!   +--> HKDF-SHA256(salt="blu-pq-v1", info="") -> 32 bytes -> PQ seed (mlkem768x25519)
//!   |
//!   +--> HKDF-SHA256(salt="blu-device-key-v1", info="") -> 32 bytes -> device key
//! ```
//!
//! The mnemonic is never stored on disk. Users must remember it or
//! use a recovery kit.

use std::str::FromStr;

use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use zeroize::ZeroizeOnDrop;

use crate::error::{BluError, Result};

/// HKDF salt for deriving the x25519 identity key from the seed.
const X25519_SALT: &[u8] = b"blu-x25519-v1";

/// HKDF salt for deriving the post-quantum identity seed.
const PQ_SALT: &[u8] = b"blu-pq-v1";

/// HKDF salt for deriving the device encryption key from the seed.
const DEVICE_KEY_SALT: &[u8] = b"blu-device-key-v1";

/// Number of words in a blu mnemonic.
pub const WORD_COUNT: usize = 24;

/// A BIP39 seed (512 bits). Zeroized on drop.
#[derive(ZeroizeOnDrop)]
pub struct Seed {
    #[zeroize]
    bytes: [u8; 64],
}

impl Seed {
    /// Create a Seed from raw bytes.
    pub fn from_bytes(bytes: [u8; 64]) -> Self {
        Self { bytes }
    }

    /// Access the raw seed bytes.
    pub fn as_bytes(&self) -> &[u8; 64] {
        &self.bytes
    }
}

/// A 32-byte derived key. Zeroized on drop.
#[derive(Clone, ZeroizeOnDrop)]
pub struct DerivedKey {
    #[zeroize]
    bytes: [u8; 32],
}

impl DerivedKey {
    /// Access the raw key bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

/// Generate a new 24-word BIP39 mnemonic.
///
/// Uses the OS CSPRNG for 256 bits of entropy (24 words).
pub fn generate_mnemonic() -> Result<bip39::Mnemonic> {
    let mut entropy = [0u8; 32]; // 256 bits = 24 words
    rand::rngs::OsRng.fill_bytes(&mut entropy);
    let mnemonic = bip39::Mnemonic::from_entropy(&entropy)
        .map_err(|e| BluError::Internal(format!("mnemonic generation failed: {}", e)))?;
    entropy.iter_mut().for_each(|b| *b = 0); // zeroize
    Ok(mnemonic)
}

/// Parse and validate a BIP39 mnemonic from a string.
pub fn parse_mnemonic(words: &str) -> Result<bip39::Mnemonic> {
    bip39::Mnemonic::parse(words)
        .map_err(|e| BluError::InvalidKeyFormat(format!("invalid mnemonic: {}", e)))
}

/// Derive a 512-bit seed from a mnemonic and optional passphrase.
///
/// Uses PBKDF2-HMAC-SHA512 with 2048 rounds per BIP39 spec.
/// The passphrase acts as a "25th word"; an empty string is valid.
pub fn mnemonic_to_seed(mnemonic: &bip39::Mnemonic, passphrase: &str) -> Seed {
    let bytes = mnemonic.to_seed(passphrase);
    Seed::from_bytes(bytes)
}

/// Derive a 32-byte key from a seed using HKDF-SHA256.
fn derive_key(seed: &Seed, salt: &[u8]) -> Result<DerivedKey> {
    let hk = Hkdf::<Sha256>::new(Some(salt), seed.as_bytes());
    let mut okm = [0u8; 32];
    hk.expand(b"", &mut okm)
        .map_err(|e| BluError::Internal(format!("HKDF expand failed: {}", e)))?;
    Ok(DerivedKey { bytes: okm })
}

/// Derive an age x25519 identity (private key) from a seed.
///
/// The 32 bytes are derived via HKDF-SHA256 with salt "blu-x25519-v1",
/// then encoded as a bech32 AGE-SECRET-KEY string and parsed into an
/// age identity.
pub fn derive_x25519_identity(seed: &Seed) -> Result<age::x25519::Identity> {
    let key = derive_key(seed, X25519_SALT)?;
    identity_from_raw_bytes(key.as_bytes())
}

/// Derive a device encryption key from a seed.
///
/// Used to encrypt the seed for biometric storage.
pub fn derive_device_key(seed: &Seed) -> Result<DerivedKey> {
    derive_key(seed, DEVICE_KEY_SALT)
}

/// Derive a post-quantum identity seed from the BIP39 seed.
///
/// The 32-byte output is the seed for the mlkem768x25519 hybrid KEM.
/// SHAKE256 expands it to derive both ML-KEM-768 and X25519 keys
/// (see `hybrid_kem::expand_seed`).
pub fn derive_pq_seed(seed: &Seed) -> Result<crate::keys::hybrid_kem::HybridSeed> {
    let key = derive_key(seed, PQ_SALT)?;
    Ok(crate::keys::hybrid_kem::HybridSeed::new(*key.as_bytes()))
}

/// Derive a post-quantum identity from the BIP39 seed.
pub fn derive_pq_identity(seed: &Seed) -> Result<crate::keys::pq::PqIdentity> {
    let pq_seed = derive_pq_seed(seed)?;
    Ok(crate::keys::pq::PqIdentity::new(pq_seed))
}

/// Derive a post-quantum recipient (public key) from the BIP39 seed.
pub fn derive_pq_recipient(seed: &Seed) -> Result<crate::keys::pq::PqRecipient> {
    let pq_seed = derive_pq_seed(seed)?;
    let pk = crate::keys::hybrid_kem::public_key_from_seed(&pq_seed);
    Ok(crate::keys::pq::PqRecipient::new(pk))
}

/// Construct an age x25519 Identity from raw 32-byte private key material.
fn identity_from_raw_bytes(bytes: &[u8; 32]) -> Result<age::x25519::Identity> {
    use bech32::{ToBase32, Variant};

    let encoded = bech32::encode("age-secret-key-", bytes.to_base32(), Variant::Bech32)
        .map_err(|e| BluError::Internal(format!("bech32 encode failed: {}", e)))?;
    let upper = encoded.to_uppercase();

    age::x25519::Identity::from_str(&upper)
        .map_err(|e| BluError::InvalidKeyFormat(format!("failed to create age identity: {}", e)))
}

/// Get the public key (recipient) string from an identity.
pub fn public_key_from_identity(identity: &age::x25519::Identity) -> String {
    identity.to_public().to_string()
}

/// Full pipeline: mnemonic string + passphrase -> age identity.
///
/// Convenience function that combines parsing, seed derivation, and
/// key derivation in one call.
pub fn identity_from_mnemonic(
    mnemonic_str: &str,
    passphrase: &str,
) -> Result<age::x25519::Identity> {
    let mnemonic = parse_mnemonic(mnemonic_str)?;
    let seed = mnemonic_to_seed(&mnemonic, passphrase);
    derive_x25519_identity(&seed)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn generate_mnemonic_24_words() {
        let m = generate_mnemonic().unwrap();
        assert_eq!(m.word_count(), 24);

        // Should be parseable
        let s = m.to_string();
        let words: Vec<&str> = s.split_whitespace().collect();
        assert_eq!(words.len(), 24);

        // Should round-trip
        let m2 = parse_mnemonic(&s).unwrap();
        assert_eq!(m.to_string(), m2.to_string());
    }

    #[test]
    fn generate_mnemonic_is_random() {
        let m1 = generate_mnemonic().unwrap();
        let m2 = generate_mnemonic().unwrap();
        assert_ne!(m1.to_string(), m2.to_string());
    }

    #[test]
    fn parse_mnemonic_valid() {
        // Standard BIP39 test vector (24 words from all-zero entropy)
        let words = "abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon art";
        let m = parse_mnemonic(words).unwrap();
        assert_eq!(m.word_count(), 24);
    }

    #[test]
    fn parse_mnemonic_invalid() {
        assert!(parse_mnemonic("not valid words").is_err());
        assert!(parse_mnemonic("").is_err());
        // Wrong checksum
        assert!(parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon"
        )
        .is_err());
    }

    #[test]
    fn seed_derivation_deterministic() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();

        let s1 = mnemonic_to_seed(&m, "");
        let s2 = mnemonic_to_seed(&m, "");
        assert_eq!(s1.as_bytes(), s2.as_bytes());
    }

    #[test]
    fn seed_differs_with_passphrase() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();

        let s1 = mnemonic_to_seed(&m, "");
        let s2 = mnemonic_to_seed(&m, "my secret passphrase");
        assert_ne!(s1.as_bytes(), s2.as_bytes());
    }

    #[test]
    fn derive_x25519_identity_deterministic() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();
        let seed = mnemonic_to_seed(&m, "");

        let id1 = derive_x25519_identity(&seed).unwrap();
        let id2 = derive_x25519_identity(&seed).unwrap();

        let pub1 = public_key_from_identity(&id1);
        let pub2 = public_key_from_identity(&id2);
        assert_eq!(pub1, pub2);
        assert!(pub1.starts_with("age1"));
    }

    #[test]
    fn derive_x25519_identity_differs_by_passphrase() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();

        let seed1 = mnemonic_to_seed(&m, "");
        let seed2 = mnemonic_to_seed(&m, "different");

        let id1 = derive_x25519_identity(&seed1).unwrap();
        let id2 = derive_x25519_identity(&seed2).unwrap();

        let pub1 = public_key_from_identity(&id1);
        let pub2 = public_key_from_identity(&id2);
        assert_ne!(pub1, pub2);
    }

    #[test]
    fn derive_device_key_deterministic() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();
        let seed = mnemonic_to_seed(&m, "");

        let dk1 = derive_device_key(&seed).unwrap();
        let dk2 = derive_device_key(&seed).unwrap();
        assert_eq!(dk1.as_bytes(), dk2.as_bytes());
    }

    #[test]
    fn derive_device_key_differs_from_x25519() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();
        let seed = mnemonic_to_seed(&m, "");

        let x25519_key = derive_key(&seed, X25519_SALT).unwrap();
        let device_key = derive_device_key(&seed).unwrap();
        assert_ne!(x25519_key.as_bytes(), device_key.as_bytes());
    }

    #[test]
    fn identity_from_mnemonic_convenience() {
        let words = "abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon art";

        let id = identity_from_mnemonic(words, "").unwrap();
        let pubkey = public_key_from_identity(&id);
        assert!(pubkey.starts_with("age1"));

        // Should be deterministic
        let id2 = identity_from_mnemonic(words, "").unwrap();
        assert_eq!(pubkey, public_key_from_identity(&id2));
    }

    #[test]
    fn identity_encrypts_decrypts() {
        let words = "abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon art";

        let id = identity_from_mnemonic(words, "test passphrase").unwrap();
        let bbox = crate::keys::blackbox_from_identity(id);

        let plaintext = b"encrypted with mnemonic-derived key";
        let encrypted = bbox.encrypt(plaintext).unwrap();
        let decrypted = bbox.decrypt(&encrypted).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn derive_pq_seed_deterministic() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();
        let seed = mnemonic_to_seed(&m, "");

        let pq1 = derive_pq_seed(&seed).unwrap();
        let pq2 = derive_pq_seed(&seed).unwrap();
        assert_eq!(pq1.as_bytes(), pq2.as_bytes());
    }

    #[test]
    fn derive_pq_seed_differs_from_x25519() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();
        let seed = mnemonic_to_seed(&m, "");

        let x25519_key = derive_key(&seed, X25519_SALT).unwrap();
        let pq_seed = derive_pq_seed(&seed).unwrap();
        assert_ne!(x25519_key.as_bytes(), pq_seed.as_bytes());
    }

    #[test]
    fn derive_pq_seed_differs_from_device_key() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();
        let seed = mnemonic_to_seed(&m, "");

        let device_key = derive_device_key(&seed).unwrap();
        let pq_seed = derive_pq_seed(&seed).unwrap();
        assert_ne!(device_key.as_bytes(), pq_seed.as_bytes());
    }

    #[test]
    fn derive_pq_identity_round_trip() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();
        let seed = mnemonic_to_seed(&m, "");

        let identity = derive_pq_identity(&seed).unwrap();
        let recipient = derive_pq_recipient(&seed).unwrap();

        // Identity should derive the same public key as the recipient
        let derived_recipient = identity.to_public();
        assert_eq!(
            derived_recipient.public_key().as_bytes(),
            recipient.public_key().as_bytes()
        );
    }

    #[test]
    fn derive_pq_identity_differs_by_passphrase() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();

        let seed1 = mnemonic_to_seed(&m, "");
        let seed2 = mnemonic_to_seed(&m, "different");

        let pq1 = derive_pq_seed(&seed1).unwrap();
        let pq2 = derive_pq_seed(&seed2).unwrap();
        assert_ne!(pq1.as_bytes(), pq2.as_bytes());
    }

    #[test]
    fn derive_pq_recipient_bech32_starts_with_age1pq() {
        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();
        let seed = mnemonic_to_seed(&m, "");

        let recipient = derive_pq_recipient(&seed).unwrap();
        let encoded = recipient.to_string();
        assert!(encoded.starts_with("age1pq"));
    }

    #[test]
    fn pq_encrypt_decrypt_from_mnemonic() {
        use age::Identity;
        use age::Recipient;
        use age_core::secrecy::ExposeSecret;

        let m = parse_mnemonic(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        )
        .unwrap();
        let seed = mnemonic_to_seed(&m, "");

        let identity = derive_pq_identity(&seed).unwrap();
        let recipient = derive_pq_recipient(&seed).unwrap();

        // Wrap a file key
        let file_key = age_core::format::FileKey::init_with_mut(|fk| {
            rand::rngs::OsRng.fill_bytes(fk);
        });

        let (stanzas, labels) = recipient.wrap_file_key(&file_key).unwrap();
        assert!(labels.contains("postquantum"));

        let recovered = identity.unwrap_stanza(&stanzas[0]).unwrap().unwrap();
        assert_eq!(recovered.expose_secret(), file_key.expose_secret());
    }
}
