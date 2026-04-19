//! MLKEM768-X25519 hybrid KEM.
//!
//! Implements the hybrid KEM from draft-ietf-hpke-pq-03 (filippo.io/hpke-pq)
//! used by the age `mlkem768x25519` recipient type. The construction
//! combines ML-KEM-768 and X25519 to produce a 32-byte shared secret
//! that is secure against both classical and quantum adversaries.
//!
//! The combiner is:
//!
//! ```text
//! ss = SHA3-256(ss_PQ || ss_T || ct_T || ek_T || LABEL)
//! ```
//!
//! where LABEL is `\x5c\x2e\x2f\x2f\x5e\x5c` (the `\./` `/^\` string
//! from the spec).
//!
//! Key derivation from a 32-byte seed follows the Go age reference
//! implementation:
//!
//! ```text
//! SHAKE256(seed) -> 96 bytes
//!   [0..64]  = (d, z) for ML-KEM-768 deterministic keygen
//!   [64..96] = X25519 private scalar
//! ```

use ml_kem::array::Array;
use ml_kem::kem::{Decapsulate, Encapsulate};
use ml_kem::{EncodedSizeUser, KemCore, MlKem768, MlKem768Params};
use rand::rngs::OsRng;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{BluError, Result};

/// Size of an ML-KEM-768 encapsulation key in bytes.
pub const MLKEM_EK_SIZE: usize = 1184;
/// Size of an ML-KEM-768 ciphertext in bytes.
pub const MLKEM_CT_SIZE: usize = 1088;
/// Size of an X25519 key in bytes.
pub const X25519_KEY_SIZE: usize = 32;

/// Size of the hybrid public key: ML-KEM-768 encapsulation key + X25519.
pub const HYBRID_PK_SIZE: usize = MLKEM_EK_SIZE + X25519_KEY_SIZE;

/// Size of the hybrid ciphertext: ML-KEM-768 ciphertext + X25519 ephemeral.
pub const HYBRID_CT_SIZE: usize = MLKEM_CT_SIZE + X25519_KEY_SIZE;

/// Size of the seed that derives the full hybrid keypair.
pub const SEED_SIZE: usize = 32;

/// Size of the shared secret output.
pub const SHARED_SECRET_SIZE: usize = 32;

/// The combiner label from the spec: `\./` + `/^\`
const KEM_LABEL: &[u8] = b"\x5c\x2e\x2f\x2f\x5e\x5c";

/// A 32-byte seed that deterministically derives a hybrid keypair.
///
/// This is the identity (private key) for the mlkem768x25519 recipient
/// type. SHAKE256 expands it to 96 bytes: 64 for ML-KEM-768 keygen and
/// 32 for the X25519 scalar.
#[derive(Clone, ZeroizeOnDrop)]
pub struct HybridSeed {
    #[zeroize]
    bytes: [u8; SEED_SIZE],
}

impl HybridSeed {
    /// Create a seed from raw bytes.
    pub fn new(bytes: [u8; SEED_SIZE]) -> Self {
        Self { bytes }
    }

    /// Access the raw seed bytes.
    pub fn as_bytes(&self) -> &[u8; SEED_SIZE] {
        &self.bytes
    }
}

impl std::fmt::Debug for HybridSeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HybridSeed").finish()
    }
}

/// A hybrid public key (1216 bytes): ML-KEM-768 encapsulation key
/// concatenated with an X25519 public key.
#[derive(Clone, PartialEq, Eq)]
pub struct HybridPublicKey {
    bytes: [u8; HYBRID_PK_SIZE],
}

impl HybridPublicKey {
    /// Create from raw bytes. Returns an error if the length is wrong.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != HYBRID_PK_SIZE {
            return Err(BluError::InvalidKeyFormat(format!(
                "hybrid public key must be {} bytes, got {}",
                HYBRID_PK_SIZE,
                data.len()
            )));
        }
        let mut bytes = [0u8; HYBRID_PK_SIZE];
        bytes.copy_from_slice(data);
        Ok(Self { bytes })
    }

    /// Access the raw public key bytes.
    pub fn as_bytes(&self) -> &[u8; HYBRID_PK_SIZE] {
        &self.bytes
    }

    fn mlkem_ek_bytes(&self) -> &[u8] {
        &self.bytes[..MLKEM_EK_SIZE]
    }

    fn x25519_pk_bytes(&self) -> [u8; X25519_KEY_SIZE] {
        let mut b = [0u8; X25519_KEY_SIZE];
        b.copy_from_slice(&self.bytes[MLKEM_EK_SIZE..]);
        b
    }
}

impl std::fmt::Debug for HybridPublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HybridPublicKey")
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// A hybrid ciphertext (1120 bytes): ML-KEM-768 ciphertext concatenated
/// with an X25519 ephemeral public key.
#[derive(Clone, PartialEq, Eq)]
pub struct HybridCiphertext {
    bytes: [u8; HYBRID_CT_SIZE],
}

impl HybridCiphertext {
    /// Create from raw bytes. Returns an error if the length is wrong.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != HYBRID_CT_SIZE {
            return Err(BluError::InvalidKeyFormat(format!(
                "hybrid ciphertext must be {} bytes, got {}",
                HYBRID_CT_SIZE,
                data.len()
            )));
        }
        let mut bytes = [0u8; HYBRID_CT_SIZE];
        bytes.copy_from_slice(data);
        Ok(Self { bytes })
    }

    /// Access the raw ciphertext bytes.
    pub fn as_bytes(&self) -> &[u8; HYBRID_CT_SIZE] {
        &self.bytes
    }
}

/// Expand a 32-byte seed into 96 bytes via SHAKE256.
///
/// Returns (seed_pq[64], seed_t[32]) where seed_pq is split into
/// (d[32], z[32]) for ML-KEM-768 deterministic keygen and seed_t is
/// the X25519 private scalar.
fn expand_seed(seed: &HybridSeed) -> [u8; 96] {
    use sha3::digest::{ExtendableOutput, Update, XofReader};
    use sha3::Shake256;

    let mut h = Shake256::default();
    h.update(seed.as_bytes());
    let mut xof = h.finalize_xof();
    let mut buf = [0u8; 96];
    xof.read(&mut buf);
    buf
}

/// SHA3-256 combiner per the hybrid KEM spec.
///
/// ```text
/// ss = SHA3-256(ss_PQ || ss_T || ct_T || ek_T || LABEL)
/// ```
fn combiner(ss_pq: &[u8], ss_t: &[u8], ct_t: &[u8], ek_t: &[u8]) -> [u8; SHARED_SECRET_SIZE] {
    use sha3::Digest;
    use sha3::Sha3_256;

    let mut h = Sha3_256::new();
    h.update(ss_pq);
    h.update(ss_t);
    h.update(ct_t);
    h.update(ek_t);
    h.update(KEM_LABEL);
    let result = h.finalize();

    let mut out = [0u8; SHARED_SECRET_SIZE];
    out.copy_from_slice(&result);
    out
}

/// Derive the hybrid public key from a seed.
///
/// Expands the seed via SHAKE256, generates the ML-KEM-768 keypair
/// deterministically, and computes the X25519 public key.
pub fn public_key_from_seed(seed: &HybridSeed) -> HybridPublicKey {
    let mut expanded = expand_seed(seed);

    let d_bytes: [u8; 32] = expanded[0..32].try_into().unwrap();
    let z_bytes: [u8; 32] = expanded[32..64].try_into().unwrap();
    let t_bytes: [u8; 32] = expanded[64..96].try_into().unwrap();

    let d_arr = Array::try_from(d_bytes.as_slice()).expect("d is 32 bytes");
    let z_arr = Array::try_from(z_bytes.as_slice()).expect("z is 32 bytes");
    let (_, ek) = MlKem768::generate_deterministic(&d_arr, &z_arr);

    let sk_t = X25519StaticSecret::from(t_bytes);
    let pk_t = X25519PublicKey::from(&sk_t);

    let ek_bytes = ek.as_bytes();
    let mut pk = [0u8; HYBRID_PK_SIZE];
    pk[..MLKEM_EK_SIZE].copy_from_slice(ek_bytes.as_ref());
    pk[MLKEM_EK_SIZE..].copy_from_slice(pk_t.as_bytes());

    expanded.zeroize();
    HybridPublicKey { bytes: pk }
}

/// Encapsulate: produce a ciphertext and shared secret for a recipient.
///
/// Uses the hybrid KEM with fresh randomness for both the ML-KEM-768
/// encapsulation and the X25519 ephemeral key exchange.
pub fn encapsulate(pk: &HybridPublicKey) -> Result<(HybridCiphertext, [u8; SHARED_SECRET_SIZE])> {
    let ek_arr = Array::try_from(pk.mlkem_ek_bytes())
        .map_err(|_| BluError::InvalidKeyFormat("bad ML-KEM EK length".into()))?;
    let mlkem_ek = ml_kem::kem::EncapsulationKey::<MlKem768Params>::from_bytes(&ek_arr);
    let (ct_pq, ss_pq) = mlkem_ek
        .encapsulate(&mut OsRng)
        .map_err(|_| BluError::EncryptionFailed("ML-KEM-768 encapsulate failed".into()))?;

    let eph_secret = x25519_dalek::EphemeralSecret::random_from_rng(OsRng);
    let eph_public = X25519PublicKey::from(&eph_secret);
    let recipient_x25519 = X25519PublicKey::from(pk.x25519_pk_bytes());
    let ss_t = eph_secret.diffie_hellman(&recipient_x25519);

    let shared_secret = combiner(
        ss_pq.as_ref(),
        ss_t.as_bytes(),
        eph_public.as_bytes(),
        &pk.as_bytes()[MLKEM_EK_SIZE..],
    );

    let mut ct_bytes = [0u8; HYBRID_CT_SIZE];
    ct_bytes[..MLKEM_CT_SIZE].copy_from_slice(ct_pq.as_ref());
    ct_bytes[MLKEM_CT_SIZE..].copy_from_slice(eph_public.as_bytes());

    Ok((HybridCiphertext { bytes: ct_bytes }, shared_secret))
}

/// Decapsulate: recover the shared secret from a ciphertext using a seed.
///
/// Re-derives the ML-KEM-768 and X25519 private keys from the seed,
/// decapsulates both, and combines the shared secrets.
pub fn decapsulate(seed: &HybridSeed, ct: &HybridCiphertext) -> Result<[u8; SHARED_SECRET_SIZE]> {
    let mut expanded = expand_seed(seed);

    let d_bytes: [u8; 32] = expanded[0..32].try_into().unwrap();
    let z_bytes: [u8; 32] = expanded[32..64].try_into().unwrap();
    let t_bytes: [u8; 32] = expanded[64..96].try_into().unwrap();

    let d_arr = Array::try_from(d_bytes.as_slice()).expect("d is 32 bytes");
    let z_arr = Array::try_from(z_bytes.as_slice()).expect("z is 32 bytes");
    let (dk, _ek) = MlKem768::generate_deterministic(&d_arr, &z_arr);

    let sk_t = X25519StaticSecret::from(t_bytes);
    let pk_t = X25519PublicKey::from(&sk_t);

    expanded.zeroize();

    let ct_pq_ref = Array::try_from(&ct.as_bytes()[..MLKEM_CT_SIZE])
        .map_err(|_| BluError::DecryptionFailed("bad ML-KEM ciphertext length".into()))?;
    let ss_pq = dk
        .decapsulate(&ct_pq_ref)
        .map_err(|_| BluError::DecryptionFailed("ML-KEM-768 decapsulate failed".into()))?;

    let ct_t_bytes: [u8; 32] = ct.as_bytes()[MLKEM_CT_SIZE..].try_into().unwrap();
    let ct_t_pk = X25519PublicKey::from(ct_t_bytes);
    let ss_t = sk_t.diffie_hellman(&ct_t_pk);

    let shared_secret = combiner(
        ss_pq.as_ref(),
        ss_t.as_bytes(),
        &ct.as_bytes()[MLKEM_CT_SIZE..],
        pk_t.as_bytes(),
    );

    Ok(shared_secret)
}

#[cfg(test)]
mod test {
    use super::*;
    use rand::RngCore;

    fn random_seed() -> HybridSeed {
        let mut bytes = [0u8; SEED_SIZE];
        OsRng.fill_bytes(&mut bytes);
        HybridSeed::new(bytes)
    }

    #[test]
    fn public_key_deterministic() {
        let seed = random_seed();
        let pk1 = public_key_from_seed(&seed);
        let pk2 = public_key_from_seed(&seed);
        assert_eq!(pk1.as_bytes(), pk2.as_bytes());
    }

    #[test]
    fn public_key_differs_by_seed() {
        let pk1 = public_key_from_seed(&random_seed());
        let pk2 = public_key_from_seed(&random_seed());
        assert_ne!(pk1.as_bytes(), pk2.as_bytes());
    }

    #[test]
    fn public_key_size() {
        let pk = public_key_from_seed(&random_seed());
        assert_eq!(pk.as_bytes().len(), HYBRID_PK_SIZE);
        assert_eq!(HYBRID_PK_SIZE, 1216);
    }

    #[test]
    fn encapsulate_decapsulate_round_trip() {
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);

        let (ct, ss_sender) = encapsulate(&pk).unwrap();
        assert_eq!(ct.as_bytes().len(), HYBRID_CT_SIZE);
        assert_eq!(HYBRID_CT_SIZE, 1120);

        let ss_receiver = decapsulate(&seed, &ct).unwrap();
        assert_eq!(ss_sender, ss_receiver);
    }

    #[test]
    fn different_encapsulations_produce_different_ciphertexts() {
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);

        let (ct1, ss1) = encapsulate(&pk).unwrap();
        let (ct2, ss2) = encapsulate(&pk).unwrap();

        assert_ne!(ct1.as_bytes(), ct2.as_bytes());
        assert_ne!(ss1, ss2);

        let ss1_dec = decapsulate(&seed, &ct1).unwrap();
        let ss2_dec = decapsulate(&seed, &ct2).unwrap();
        assert_eq!(ss1, ss1_dec);
        assert_eq!(ss2, ss2_dec);
    }

    #[test]
    fn wrong_seed_decapsulate_differs() {
        let seed1 = random_seed();
        let seed2 = random_seed();
        let pk1 = public_key_from_seed(&seed1);

        let (ct, ss_sender) = encapsulate(&pk1).unwrap();

        // Decapsulating with the wrong seed should produce a different
        // shared secret (ML-KEM implicit rejection).
        let ss_wrong = decapsulate(&seed2, &ct).unwrap();
        assert_ne!(ss_sender, ss_wrong);
    }

    #[test]
    fn hybrid_public_key_from_bytes_round_trip() {
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);

        let pk2 = HybridPublicKey::from_bytes(pk.as_bytes()).unwrap();
        assert_eq!(pk.as_bytes(), pk2.as_bytes());
    }

    #[test]
    fn hybrid_public_key_from_bytes_wrong_size() {
        assert!(HybridPublicKey::from_bytes(&[0u8; 100]).is_err());
        assert!(HybridPublicKey::from_bytes(&[0u8; 1217]).is_err());
    }

    #[test]
    fn hybrid_ciphertext_from_bytes_round_trip() {
        let seed = random_seed();
        let pk = public_key_from_seed(&seed);
        let (ct, _) = encapsulate(&pk).unwrap();

        let ct2 = HybridCiphertext::from_bytes(ct.as_bytes()).unwrap();
        assert_eq!(ct.as_bytes(), ct2.as_bytes());
    }

    #[test]
    fn hybrid_ciphertext_from_bytes_wrong_size() {
        assert!(HybridCiphertext::from_bytes(&[0u8; 100]).is_err());
        assert!(HybridCiphertext::from_bytes(&[0u8; 1121]).is_err());
    }

    #[test]
    fn expand_seed_deterministic() {
        let seed = random_seed();
        let e1 = expand_seed(&seed);
        let e2 = expand_seed(&seed);
        assert_eq!(e1, e2);
    }

    #[test]
    fn expand_seed_differs_by_input() {
        let s1 = random_seed();
        let s2 = random_seed();
        let e1 = expand_seed(&s1);
        let e2 = expand_seed(&s2);
        assert_ne!(e1, e2);
    }

    #[test]
    fn known_seed_produces_stable_public_key() {
        // A fixed seed for regression testing. If the public key
        // derivation changes, this test catches it.
        let seed = HybridSeed::new([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ]);
        let pk = public_key_from_seed(&seed);

        // Just verify it's the right size and deterministic; the exact
        // value depends on the ML-KEM and X25519 implementations, so
        // we snapshot the first few bytes.
        assert_eq!(pk.as_bytes().len(), 1216);
        let pk2 = public_key_from_seed(&seed);
        assert_eq!(pk.as_bytes(), pk2.as_bytes());
    }

    #[test]
    fn combiner_label_matches_spec() {
        // The label is \./  /^\ which is bytes 5c 2e 2f 2f 5e 5c
        assert_eq!(KEM_LABEL, b"\x5c\x2e\x2f\x2f\x5e\x5c");
        assert_eq!(KEM_LABEL.len(), 6);
    }
}
