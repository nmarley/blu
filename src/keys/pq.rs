//! Post-quantum age recipient and identity for the `mlkem768x25519` type.
//!
//! Implements the `age::Recipient` and `age::Identity` traits for the
//! MLKEM768-X25519 hybrid post-quantum recipient type defined in the
//! C2SP age spec v1.1.0. This produces stanzas that are interoperable
//! with Go age v1.3.1 and future Rust rage releases.
//!
//! Key encoding follows the Go age convention:
//!
//! - Recipient (public key): bech32 with HRP `age1pq`
//! - Identity (seed):        bech32 with HRP `AGE-SECRET-KEY-PQ-`

use std::collections::HashSet;

use age::DecryptError;
use age_core::format::{FileKey, Stanza, FILE_KEY_BYTES};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;

use crate::error::{BluError, Result};
use crate::keys::hpke;
use crate::keys::hybrid_kem::{public_key_from_seed, HybridPublicKey, HybridSeed, HYBRID_CT_SIZE};

/// The stanza tag for mlkem768x25519 recipients.
const STANZA_TAG: &str = "mlkem768x25519";

/// The HPKE info string for the mlkem768x25519 recipient type.
const HPKE_INFO: &[u8] = b"age-encryption.org/mlkem768x25519";

/// Bech32 HRP for the public key (recipient).
const RECIPIENT_HRP: &str = "age1pq";

/// Bech32 HRP for the secret key (identity).
const IDENTITY_HRP: &str = "age-secret-key-pq-";

/// Expected size of the stanza body: 16-byte file key + 16-byte tag.
const STANZA_BODY_SIZE: usize = FILE_KEY_BYTES + 16;

/// A post-quantum age recipient (public key).
///
/// Wraps a `HybridPublicKey` and implements `age::Recipient` to produce
/// `mlkem768x25519` stanzas.
#[derive(Clone, Debug)]
pub struct PqRecipient {
    pk: HybridPublicKey,
}

impl PqRecipient {
    /// Create from a `HybridPublicKey`.
    pub fn new(pk: HybridPublicKey) -> Self {
        Self { pk }
    }

    /// Access the underlying public key.
    pub fn public_key(&self) -> &HybridPublicKey {
        &self.pk
    }

    /// Encode as a bech32 string with HRP `age1pq`.
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        use bech32::{ToBase32, Variant};
        bech32::encode(
            RECIPIENT_HRP,
            self.pk.as_bytes().to_base32(),
            Variant::Bech32,
        )
        .expect("bech32 encode should not fail for valid data")
    }
}

/// Parse a PQ recipient from a bech32 `age1pq...` string.
pub fn parse_pq_recipient(s: &str) -> Result<PqRecipient> {
    use bech32::FromBase32;

    let (hrp, data, _variant) = bech32::decode(s)
        .map_err(|e| BluError::InvalidKeyFormat(format!("invalid PQ recipient: {}", e)))?;

    if hrp.to_lowercase() != RECIPIENT_HRP {
        return Err(BluError::InvalidKeyFormat(format!(
            "expected HRP '{}', got '{}'",
            RECIPIENT_HRP, hrp
        )));
    }

    let bytes = Vec::<u8>::from_base32(&data)
        .map_err(|e| BluError::InvalidKeyFormat(format!("invalid PQ recipient data: {}", e)))?;

    let pk = HybridPublicKey::from_bytes(&bytes)?;
    Ok(PqRecipient::new(pk))
}

impl age::Recipient for PqRecipient {
    fn wrap_file_key(
        &self,
        file_key: &FileKey,
    ) -> std::result::Result<(Vec<Stanza>, HashSet<String>), age::EncryptError> {
        use age_core::secrecy::ExposeSecret;

        let (enc, ct) = hpke::seal_base(&self.pk, HPKE_INFO, b"", file_key.expose_secret())
            .map_err(|e| age::EncryptError::from(std::io::Error::other(e.to_string())))?;

        let stanza = Stanza {
            tag: STANZA_TAG.to_string(),
            args: vec![STANDARD_NO_PAD.encode(&enc)],
            body: ct,
        };

        let mut labels = HashSet::new();
        labels.insert("postquantum".to_string());

        Ok((vec![stanza], labels))
    }
}

/// A post-quantum age identity (secret key seed).
///
/// Wraps a `HybridSeed` and implements `age::Identity` to decrypt
/// `mlkem768x25519` stanzas.
#[derive(Clone, Debug)]
pub struct PqIdentity {
    seed: HybridSeed,
}

impl PqIdentity {
    /// Create from a `HybridSeed`.
    pub fn new(seed: HybridSeed) -> Self {
        Self { seed }
    }

    /// Access the underlying seed.
    pub fn seed(&self) -> &HybridSeed {
        &self.seed
    }

    /// Derive the corresponding public recipient.
    pub fn to_public(&self) -> PqRecipient {
        PqRecipient::new(public_key_from_seed(&self.seed))
    }

    /// Encode as a bech32 string with HRP `AGE-SECRET-KEY-PQ-`.
    pub fn to_bech32(&self) -> String {
        use bech32::{ToBase32, Variant};
        let encoded = bech32::encode(
            IDENTITY_HRP,
            self.seed.as_bytes().to_base32(),
            Variant::Bech32,
        )
        .expect("bech32 encode should not fail for valid data");
        encoded.to_uppercase()
    }
}

/// Parse a PQ identity from a bech32 `AGE-SECRET-KEY-PQ-...` string.
pub fn parse_pq_identity(s: &str) -> Result<PqIdentity> {
    use bech32::FromBase32;

    let (hrp, data, _variant) = bech32::decode(s)
        .map_err(|e| BluError::InvalidKeyFormat(format!("invalid PQ identity: {}", e)))?;

    if hrp.to_lowercase() != IDENTITY_HRP {
        return Err(BluError::InvalidKeyFormat(format!(
            "expected HRP '{}', got '{}'",
            IDENTITY_HRP, hrp
        )));
    }

    let bytes = Vec::<u8>::from_base32(&data)
        .map_err(|e| BluError::InvalidKeyFormat(format!("invalid PQ identity data: {}", e)))?;

    if bytes.len() != 32 {
        return Err(BluError::InvalidKeyFormat(format!(
            "PQ identity seed must be 32 bytes, got {}",
            bytes.len()
        )));
    }

    let mut seed_bytes = [0u8; 32];
    seed_bytes.copy_from_slice(&bytes);
    Ok(PqIdentity::new(HybridSeed::new(seed_bytes)))
}

impl age::Identity for PqIdentity {
    fn unwrap_stanza(&self, stanza: &Stanza) -> Option<std::result::Result<FileKey, DecryptError>> {
        if stanza.tag != STANZA_TAG {
            return None;
        }

        // Validate argument count per spec
        if stanza.args.len() != 1 {
            return Some(Err(DecryptError::InvalidHeader));
        }

        // Decode the enc (KEM ciphertext) from base64
        let enc = match STANDARD_NO_PAD.decode(&stanza.args[0]) {
            Ok(data) => data,
            Err(_) => return Some(Err(DecryptError::InvalidHeader)),
        };

        // Validate enc size per spec
        if enc.len() != HYBRID_CT_SIZE {
            return Some(Err(DecryptError::InvalidHeader));
        }

        // Validate body size per spec (mitigates partitioning oracle attacks)
        if stanza.body.len() != STANZA_BODY_SIZE {
            return Some(Err(DecryptError::InvalidHeader));
        }

        // Decrypt
        match hpke::open_base(&self.seed, &enc, HPKE_INFO, b"", &stanza.body) {
            Ok(pt) => {
                if pt.len() != FILE_KEY_BYTES {
                    return Some(Err(DecryptError::InvalidHeader));
                }
                let fk = FileKey::init_with_mut(|fk| fk.copy_from_slice(&pt));
                Some(Ok(fk))
            }
            Err(_) => Some(Err(DecryptError::DecryptionFailed)),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use age::Identity;
    use age::Recipient;
    use age_core::secrecy::ExposeSecret;
    use rand::RngCore;

    fn random_seed() -> HybridSeed {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        HybridSeed::new(bytes)
    }

    #[test]
    fn recipient_bech32_round_trip() {
        let seed = random_seed();
        let recipient = PqRecipient::new(public_key_from_seed(&seed));
        let encoded = recipient.to_string();

        assert!(encoded.starts_with(RECIPIENT_HRP));

        let parsed = parse_pq_recipient(&encoded).unwrap();
        assert_eq!(parsed.pk.as_bytes(), recipient.pk.as_bytes());
    }

    #[test]
    fn identity_bech32_round_trip() {
        let seed = random_seed();
        let identity = PqIdentity::new(seed.clone());
        let encoded = identity.to_bech32();

        assert!(encoded.starts_with("AGE-SECRET-KEY-PQ-"));

        let parsed = parse_pq_identity(&encoded).unwrap();
        assert_eq!(parsed.seed.as_bytes(), identity.seed.as_bytes());
    }

    #[test]
    fn identity_derives_matching_recipient() {
        let seed = random_seed();
        let identity = PqIdentity::new(seed.clone());
        let recipient = PqRecipient::new(public_key_from_seed(&seed));

        let derived = identity.to_public();
        assert_eq!(derived.pk.as_bytes(), recipient.pk.as_bytes());
    }

    #[test]
    fn wrap_unwrap_file_key() {
        let seed = random_seed();
        let recipient = PqRecipient::new(public_key_from_seed(&seed));
        let identity = PqIdentity::new(seed);

        // Create a file key
        let file_key = FileKey::init_with_mut(|fk| {
            rand::rngs::OsRng.fill_bytes(fk);
        });

        // Wrap
        let (stanzas, labels) = recipient.wrap_file_key(&file_key).unwrap();
        assert_eq!(stanzas.len(), 1);
        assert_eq!(stanzas[0].tag, STANZA_TAG);
        assert_eq!(stanzas[0].args.len(), 1);
        assert_eq!(stanzas[0].body.len(), STANZA_BODY_SIZE);
        assert!(labels.contains("postquantum"));

        // Unwrap
        let result = identity.unwrap_stanza(&stanzas[0]);
        assert!(result.is_some());
        let recovered = result.unwrap().unwrap();
        assert_eq!(recovered.expose_secret(), file_key.expose_secret());
    }

    #[test]
    fn unwrap_ignores_wrong_tag() {
        let identity = PqIdentity::new(random_seed());
        let stanza = Stanza {
            tag: "X25519".to_string(),
            args: vec!["foo".to_string()],
            body: vec![0u8; 32],
        };
        assert!(identity.unwrap_stanza(&stanza).is_none());
    }

    #[test]
    fn unwrap_rejects_wrong_arg_count() {
        let identity = PqIdentity::new(random_seed());
        let stanza = Stanza {
            tag: STANZA_TAG.to_string(),
            args: vec!["a".to_string(), "b".to_string()],
            body: vec![0u8; STANZA_BODY_SIZE],
        };
        let result = identity.unwrap_stanza(&stanza);
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn unwrap_rejects_wrong_body_size() {
        let identity = PqIdentity::new(random_seed());
        let stanza = Stanza {
            tag: STANZA_TAG.to_string(),
            args: vec![STANDARD_NO_PAD.encode([0u8; HYBRID_CT_SIZE])],
            body: vec![0u8; 16], // too short
        };
        let result = identity.unwrap_stanza(&stanza);
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn unwrap_wrong_seed_fails() {
        let seed1 = random_seed();
        let seed2 = random_seed();
        let recipient = PqRecipient::new(public_key_from_seed(&seed1));
        let identity2 = PqIdentity::new(seed2);

        let file_key = FileKey::init_with_mut(|fk| {
            rand::rngs::OsRng.fill_bytes(fk);
        });

        let (stanzas, _) = recipient.wrap_file_key(&file_key).unwrap();
        let result = identity2.unwrap_stanza(&stanzas[0]);
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn age_encrypt_decrypt_round_trip() {
        use std::io::{Read, Write};

        let seed = random_seed();
        let recipient = PqRecipient::new(public_key_from_seed(&seed));
        let identity = PqIdentity::new(seed);
        let plaintext = b"most excellent post-quantum encryption";

        // Encrypt using age::Encryptor
        let recipients: Vec<&dyn age::Recipient> = vec![&recipient];
        let encryptor = age::Encryptor::with_recipients(recipients.into_iter()).unwrap();
        let mut encrypted = vec![];
        let mut writer = encryptor.wrap_output(&mut encrypted).unwrap();
        writer.write_all(plaintext).unwrap();
        writer.finish().unwrap();

        // Decrypt using age::Decryptor
        let decryptor = age::Decryptor::new(&encrypted[..]).unwrap();
        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .unwrap();
        let mut decrypted = vec![];
        reader.read_to_end(&mut decrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn parse_pq_recipient_invalid() {
        assert!(parse_pq_recipient("not-valid").is_err());
        assert!(parse_pq_recipient("age1abc").is_err()); // wrong HRP
    }

    #[test]
    fn parse_pq_identity_invalid() {
        assert!(parse_pq_identity("not-valid").is_err());
        assert!(parse_pq_identity("AGE-SECRET-KEY-1ABCDEF").is_err()); // wrong HRP
    }

    #[test]
    fn enc_base64_is_correct_size() {
        let seed = random_seed();
        let recipient = PqRecipient::new(public_key_from_seed(&seed));

        let file_key = FileKey::init_with_mut(|fk| {
            rand::rngs::OsRng.fill_bytes(fk);
        });

        let (stanzas, _) = recipient.wrap_file_key(&file_key).unwrap();
        let enc_bytes = STANDARD_NO_PAD.decode(&stanzas[0].args[0]).unwrap();
        assert_eq!(enc_bytes.len(), HYBRID_CT_SIZE);
    }
}
