//! HPKE Base mode for the MLKEM768-X25519 suite.
//!
//! Implements the RFC 9180 key schedule and SealBase/OpenBase operations
//! for the HPKE suite used by the age `mlkem768x25519` recipient type:
//!
//! ```text
//! KEM:  MLKEM768-X25519  (KEM ID 0x647a)
//! KDF:  HKDF-SHA256      (KDF ID 0x0001)
//! AEAD: ChaCha20Poly1305 (AEAD ID 0x0003)
//! ```
//!
//! The key schedule follows RFC 9180 Section 5, using Labeled Extract
//! and Labeled Expand with the suite-specific `suite_id`.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce as AeadNonce};
use hkdf::Hkdf;
use sha2::Sha256;

use crate::error::{BluError, Result};
use crate::keys::hybrid_kem::{
    self, HybridCiphertext, HybridPublicKey, HybridSeed, SHARED_SECRET_SIZE,
};

/// KEM ID for MLKEM768-X25519 (draft-ietf-hpke-pq-03).
const KEM_ID: [u8; 2] = [0x64, 0x7a];

/// KDF ID for HKDF-SHA256.
const KDF_ID: [u8; 2] = [0x00, 0x01];

/// AEAD ID for ChaCha20Poly1305.
const AEAD_ID: [u8; 2] = [0x00, 0x03];

/// ChaCha20-Poly1305 key size in bytes.
const NK: usize = 32;

/// ChaCha20-Poly1305 nonce size in bytes.
const NN: usize = 12;

/// HPKE version label.
const HPKE_LABEL: &[u8] = b"HPKE-v1";

/// Build the suite_id: `"HPKE" || KEM_ID || KDF_ID || AEAD_ID`.
fn suite_id() -> [u8; 10] {
    let mut id = [0u8; 10];
    id[0..4].copy_from_slice(b"HPKE");
    id[4..6].copy_from_slice(&KEM_ID);
    id[6..8].copy_from_slice(&KDF_ID);
    id[8..10].copy_from_slice(&AEAD_ID);
    id
}

/// HPKE Labeled Extract (RFC 9180 Section 4).
///
/// ```text
/// labeled_ikm = "HPKE-v1" || suite_id || label || ikm
/// return HKDF-Extract(salt, labeled_ikm)
/// ```
///
/// Returns the 32-byte PRK. Uses `Hkdf::new()` which performs the
/// extract step (HMAC-SHA256), then extracts the PRK bytes by
/// expanding with empty info for one block.
fn labeled_extract(salt: &[u8], label: &[u8], ikm: &[u8]) -> [u8; 32] {
    let sid = suite_id();
    let mut labeled_ikm =
        Vec::with_capacity(HPKE_LABEL.len() + sid.len() + label.len() + ikm.len());
    labeled_ikm.extend_from_slice(HPKE_LABEL);
    labeled_ikm.extend_from_slice(&sid);
    labeled_ikm.extend_from_slice(label);
    labeled_ikm.extend_from_slice(ikm);

    let salt_opt = if salt.is_empty() { None } else { Some(salt) };

    // Hkdf::new does HKDF-Extract (HMAC-SHA256(salt, ikm) -> PRK).
    // To get the raw PRK bytes, we use Hkdf::extract directly which
    // returns (PRK, HKDF) but the hkdf crate doesn't expose the PRK
    // directly from new(). Instead we use the raw hmac module via hkdf.
    //
    // The hkdf crate provides `Hkdf::extract()` which returns
    // `(Output<D>, Hkdf<D>)`. But that's only in the SimpleHkdf variant.
    // Simplest: use hmac directly through the sha2-based HMAC.
    let (prk_output, _hkdf) = Hkdf::<Sha256>::extract(salt_opt, &labeled_ikm);

    let mut prk = [0u8; 32];
    prk.copy_from_slice(&prk_output);
    prk
}

/// HPKE Labeled Expand (RFC 9180 Section 4).
///
/// ```text
/// labeled_info = I2OSP(L, 2) || "HPKE-v1" || suite_id || label || info
/// return HKDF-Expand(prk, labeled_info, L)
/// ```
fn labeled_expand(prk: &[u8; 32], label: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let sid = suite_id();
    let l_bytes = (len as u16).to_be_bytes();

    let mut labeled_info =
        Vec::with_capacity(2 + HPKE_LABEL.len() + sid.len() + label.len() + info.len());
    labeled_info.extend_from_slice(&l_bytes);
    labeled_info.extend_from_slice(HPKE_LABEL);
    labeled_info.extend_from_slice(&sid);
    labeled_info.extend_from_slice(label);
    labeled_info.extend_from_slice(info);

    // Use the PRK with HKDF-Expand
    let hk = Hkdf::<Sha256>::from_prk(prk).expect("PRK is valid length");
    let mut okm = vec![0u8; len];
    hk.expand(&labeled_info, &mut okm)
        .expect("output length is valid");
    okm
}

/// Derive the AEAD key and base nonce from a shared secret and info
/// string, following the HPKE Base mode key schedule (RFC 9180 Section 5.1).
///
/// Returns (key[32], base_nonce[12]).
fn key_schedule_base(
    shared_secret: &[u8; SHARED_SECRET_SIZE],
    info: &[u8],
) -> ([u8; NK], [u8; NN]) {
    // mode = 0 (Base)
    let mode: u8 = 0;

    let psk_id_hash = labeled_extract(b"", b"psk_id_hash", b"");
    let info_hash = labeled_extract(b"", b"info_hash", info);

    // ks_context = mode || psk_id_hash || info_hash
    let mut ks_context = Vec::with_capacity(1 + 32 + 32);
    ks_context.push(mode);
    ks_context.extend_from_slice(&psk_id_hash);
    ks_context.extend_from_slice(&info_hash);

    // secret = LabeledExtract(shared_secret, "secret", default_psk="")
    let secret = labeled_extract(shared_secret, b"secret", b"");

    // key = LabeledExpand(secret, "key", ks_context, Nk)
    let key_vec = labeled_expand(&secret, b"key", &ks_context, NK);
    let mut key = [0u8; NK];
    key.copy_from_slice(&key_vec);

    // base_nonce = LabeledExpand(secret, "base_nonce", ks_context, Nn)
    let nonce_vec = labeled_expand(&secret, b"base_nonce", &ks_context, NN);
    let mut base_nonce = [0u8; NN];
    base_nonce.copy_from_slice(&nonce_vec);

    (key, base_nonce)
}

/// HPKE SealBase: encrypt a plaintext for a recipient's public key.
///
/// Performs KEM encapsulation, derives the AEAD key via the HPKE key
/// schedule, and encrypts the plaintext with ChaCha20-Poly1305.
///
/// Returns (enc, ciphertext) where enc is the 1120-byte KEM ciphertext
/// and ciphertext is the AEAD output (plaintext.len() + 16 tag bytes).
pub fn seal_base(
    pk: &HybridPublicKey,
    info: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<(Vec<u8>, Vec<u8>)> {
    let (kem_ct, shared_secret) = hybrid_kem::encapsulate(pk)?;

    let (key, base_nonce) = key_schedule_base(&shared_secret, info);

    let cipher = ChaCha20Poly1305::new((&key).into());
    let nonce = AeadNonce::from_slice(&base_nonce);

    let ct = cipher
        .encrypt(
            nonce,
            chacha20poly1305::aead::Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|e| BluError::EncryptionFailed(format!("HPKE seal: {}", e)))?;

    Ok((kem_ct.as_bytes().to_vec(), ct))
}

/// HPKE OpenBase: decrypt a ciphertext using an identity seed.
///
/// Performs KEM decapsulation from the seed, derives the AEAD key via
/// the HPKE key schedule, and decrypts the ciphertext.
///
/// The `enc` parameter is the 1120-byte KEM ciphertext from the stanza.
pub fn open_base(
    seed: &HybridSeed,
    enc: &[u8],
    info: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    let kem_ct = HybridCiphertext::from_bytes(enc)?;

    let shared_secret = hybrid_kem::decapsulate(seed, &kem_ct)?;

    let (key, base_nonce) = key_schedule_base(&shared_secret, info);

    let cipher = ChaCha20Poly1305::new((&key).into());
    let nonce = AeadNonce::from_slice(&base_nonce);

    cipher
        .decrypt(
            nonce,
            chacha20poly1305::aead::Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| BluError::DecryptionFailed("HPKE open: authentication failed".into()))
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::keys::hybrid_kem::{public_key_from_seed, HybridSeed, HYBRID_CT_SIZE};
    const AGE_INFO: &[u8] = b"age-encryption.org/mlkem768x25519";

    fn random_seed() -> HybridSeed {
        let mut bytes = [0u8; 32];
        rand::fill(&mut bytes);
        HybridSeed::new(bytes)
    }

    #[test]
    fn seal_open_round_trip() {
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);
        let plaintext = b"hello post-quantum world";

        let (enc, ct) = seal_base(&pk, AGE_INFO, b"", plaintext).unwrap();
        assert_eq!(enc.len(), HYBRID_CT_SIZE);
        assert_eq!(ct.len(), plaintext.len() + 16); // +16 for Poly1305 tag

        let pt = open_base(&seed, &enc, AGE_INFO, b"", &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn seal_open_16_byte_file_key() {
        // The age file key is exactly 16 bytes.
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);
        let file_key = [0x42u8; 16];

        let (enc, ct) = seal_base(&pk, AGE_INFO, b"", &file_key).unwrap();
        assert_eq!(ct.len(), 32); // 16 + 16 tag

        let pt = open_base(&seed, &enc, AGE_INFO, b"", &ct).unwrap();
        assert_eq!(pt, file_key);
    }

    #[test]
    fn seal_open_empty_plaintext() {
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);

        let (enc, ct) = seal_base(&pk, AGE_INFO, b"", b"").unwrap();
        assert_eq!(ct.len(), 16); // tag only

        let pt = open_base(&seed, &enc, AGE_INFO, b"", &ct).unwrap();
        assert!(pt.is_empty());
    }

    #[test]
    fn wrong_seed_fails() {
        let seed1 = random_seed();
        let seed2 = random_seed();
        let pk1 = public_key_from_seed(&seed1);

        let (enc, ct) = seal_base(&pk1, AGE_INFO, b"", b"secret").unwrap();

        let result = open_base(&seed2, &enc, AGE_INFO, b"", &ct);
        assert!(result.is_err());
    }

    #[test]
    fn wrong_info_fails() {
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);

        let (enc, ct) = seal_base(&pk, AGE_INFO, b"", b"secret").unwrap();

        let result = open_base(&seed, &enc, b"wrong-info", b"", &ct);
        assert!(result.is_err());
    }

    #[test]
    fn wrong_aad_fails() {
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);

        let (enc, ct) = seal_base(&pk, AGE_INFO, b"aad1", b"secret").unwrap();

        let result = open_base(&seed, &enc, AGE_INFO, b"aad2", &ct);
        assert!(result.is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);

        let (enc, mut ct) = seal_base(&pk, AGE_INFO, b"", b"secret").unwrap();
        ct[0] ^= 0xff;

        let result = open_base(&seed, &enc, AGE_INFO, b"", &ct);
        assert!(result.is_err());
    }

    #[test]
    fn tampered_enc_fails() {
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);

        let (mut enc, ct) = seal_base(&pk, AGE_INFO, b"", b"secret").unwrap();
        enc[500] ^= 0xff;

        let result = open_base(&seed, &enc, AGE_INFO, b"", &ct);
        assert!(result.is_err());
    }

    #[test]
    fn truncated_enc_fails() {
        let seed = random_seed();
        let result = open_base(&seed, &[0u8; 100], AGE_INFO, b"", &[0u8; 32]);
        assert!(result.is_err());
    }

    #[test]
    fn different_seals_produce_different_output() {
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);

        let (enc1, ct1) = seal_base(&pk, AGE_INFO, b"", b"same").unwrap();
        let (enc2, ct2) = seal_base(&pk, AGE_INFO, b"", b"same").unwrap();

        // Different KEM randomness each time
        assert_ne!(enc1, enc2);
        assert_ne!(ct1, ct2);

        // Both decrypt correctly
        let pt1 = open_base(&seed, &enc1, AGE_INFO, b"", &ct1).unwrap();
        let pt2 = open_base(&seed, &enc2, AGE_INFO, b"", &ct2).unwrap();
        assert_eq!(pt1, b"same");
        assert_eq!(pt2, b"same");
    }

    #[test]
    fn suite_id_bytes() {
        let sid = suite_id();
        assert_eq!(&sid[0..4], b"HPKE");
        assert_eq!(&sid[4..6], &[0x64, 0x7a]);
        assert_eq!(&sid[6..8], &[0x00, 0x01]);
        assert_eq!(&sid[8..10], &[0x00, 0x03]);
    }

    #[test]
    fn key_schedule_deterministic() {
        // Same shared_secret + info must produce same key + nonce
        let ss = [0xABu8; 32];
        let (k1, n1) = key_schedule_base(&ss, AGE_INFO);
        let (k2, n2) = key_schedule_base(&ss, AGE_INFO);
        assert_eq!(k1, k2);
        assert_eq!(n1, n2);
    }

    #[test]
    fn key_schedule_differs_by_info() {
        let ss = [0xABu8; 32];
        let (k1, _) = key_schedule_base(&ss, b"info-a");
        let (k2, _) = key_schedule_base(&ss, b"info-b");
        assert_ne!(k1, k2);
    }

    #[test]
    fn key_schedule_differs_by_shared_secret() {
        let ss1 = [0xAAu8; 32];
        let ss2 = [0xBBu8; 32];
        let (k1, _) = key_schedule_base(&ss1, AGE_INFO);
        let (k2, _) = key_schedule_base(&ss2, AGE_INFO);
        assert_ne!(k1, k2);
    }
}
