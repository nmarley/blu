# Post-Quantum Roadmap

This plan supersedes the Stage 2e (migration) sections of PLAN.md and
STAGE2.md. The `blu migrate` command is removed from the roadmap;
this project has never had a public release, so v1-to-v2 migration is
unnecessary. The v1/v2 auto-detection code stays for backward compat.

Written April 2026 after a full re-evaluation of the project state.

## Context

All three original stages from PLAN.md are implemented on the `biom`
branch (20 commits ahead of master):

- Stage 1: Agent daemon with session management and auto-lock
- Stage 2: KEK/DEK envelope encryption with versioned rotation
- Stage 3: BIP39 identity with macOS Touch ID unlock

The key hierarchy is:

    BIP39 mnemonic (24 words)
      -> PBKDF2-HMAC-SHA512 (seed)
        -> HKDF-SHA256 "blu-x25519-v1"     -> X25519 User Key (UK)
        -> HKDF-SHA256 "blu-device-key-v1"  -> Device Key (biometric)
        -> HKDF-SHA256 "blu-pq-v1"          -> PQ Key (this plan)

    UK wraps KEK  (via age, X25519+ChaCha20-Poly1305)
      KEK wraps DEK  (ChaCha20-Poly1305, per-file)
        DEK encrypts data  (ChaCha20-Poly1305)

The symmetric layers (KEK->DEK, DEK->data) are already quantum-safe
(256-bit keys). The vulnerability is the asymmetric UK->KEK layer,
which uses X25519. A Harvest Now, Decrypt Later adversary who captures
wrapped KEK blobs today can recover the KEK with a future quantum
computer, then decrypt everything.

Filippo Valsorda's April 2026 assessment: non-PQ key exchange should
be "considered a potential active compromise." Google's internal
deadline for PQ migration is 2029.

## Approach: Forward-Compatible with the age Spec

Go age v1.3.1 shipped native PQ support (Dec 2025). The C2SP age
spec v1.1.0 (March 2026) defines the `mlkem768x25519` recipient type
using HPKE with an ML-KEM-768 + X25519 hybrid KEM (the X-Wing
construction).

The Rust `age` crate (0.11.2) does not implement PQ recipients yet
(targeting rage 0.13, timeline unknown). However, the `Recipient` and
`Identity` traits are open: we implement them ourselves, producing
spec-compliant `mlkem768x25519` stanzas. This means:

- Our wrapped KEK files are valid age files
- Forward-compatible with rage 0.13 (swap to native impl when it ships)
- Interoperable with Go age v1.3.1
- No custom wire format, no future migration

We use stable crates only: `ml-kem 0.2.3` + existing `x25519-dalek
2.x` + `sha3 0.10`. No pre-release dependencies.


## Stage 0: Strike `blu migrate` from Documentation

Remove all references to the `blu migrate` command, Stage 2e, and
v1-to-v2 migration workflow from planning and design docs.

Files to edit:

  PLAN.md
    Remove lines 463-474 (Migration v1->v2 section)
    Remove line 627 (2e row in roadmap table)
    Remove lines 684-698 (Migration Path section)

  STAGE2.md
    Remove line 60 (2e row in roadmap table)
    Remove lines 601-634 (entire 2e section)
    Remove lines 671-674 (file touchlist rows for migrate)
    Remove lines 712-740 (implementation order + open questions)

  ENVELOPE_ENCRYPTION_DESIGN.md
    Remove lines 734-751 (migration path section)
    Keep lines 425-431, 455-483 (KEK rotation re-wrap; that is
      rotation, not v1-to-v2 migration)

  PLAN-v0.5.md
    Remove line 229 (migration bullet)
    Remove lines 251-263 (config migration section)

Not changed:
  - v2format.rs / age.rs: is_v2() detection and decrypt_auto() stay
  - kek.rs: KekStatus::Archived stays (KEK rotation lifecycle)
  - Error messages "vault not migrated?" should be reworded to
    "no KEK available" since they are now permanent states


## Stage 1: Post-Quantum Hybrid KEM

### 1a: Dependencies and hybrid KEM module

New dependencies in Cargo.toml:

    ml-kem = { version = "0.2.3", features = ["deterministic", "zeroize"] }
    sha3 = "0.10"

Existing deps already present: hkdf, sha2, chacha20poly1305,
x25519-dalek (transitive via age), bech32, zeroize.

New file: src/keys/hybrid_kem.rs

Implements the MLKEM768-X25519 hybrid KEM per filippo.io/hpke-pq
and draft-ietf-hpke-pq-03:

  expandKey(seed: &[u8; 32])
    SHAKE256(seed) -> 96 bytes
    seed_PQ = bytes[0..64]   (d = [0..32], z = [32..64])
    seed_T  = bytes[64..96]  (X25519 private scalar)
    (ek_PQ, dk_PQ) = MlKem768::generate_deterministic(d, z)
    ek_T = X25519(seed_T, basepoint)
    public key = ek_PQ || ek_T  (1184 + 32 = 1216 bytes)

  encapsulate(ek: &[u8; 1216])
    (ss_PQ, ct_PQ) = ML-KEM-768 Encaps(ek_PQ)
    sk_E = random(32)
    ct_T = X25519(sk_E, basepoint)
    ss_T = X25519(sk_E, ek_T)
    ss = SHA3-256(ss_PQ || ss_T || ct_T || ek_T || LABEL)
    ct = ct_PQ || ct_T   (1088 + 32 = 1120 bytes)

  decapsulate(seed: &[u8; 32], ct: &[u8; 1120])
    expand seed to get dk_PQ, dk_T, ek_T
    ss_PQ = ML-KEM-768 Decaps(dk_PQ, ct_PQ)
    ss_T  = X25519(dk_T, ct_T)
    ss = SHA3-256(ss_PQ || ss_T || ct_T || ek_T || LABEL)

  LABEL = b"\x5c\x2e\x2f\x2f\x5e\x5c"   (the \./  /^\ from the spec)

Types:
  HybridPublicKey([u8; 1216])
  HybridSeed([u8; 32])  with ZeroizeOnDrop

### 1b: HPKE Base mode

New file: src/keys/hpke.rs

Implements HPKE Base mode KeySchedule (RFC 9180) for the suite:
  KEM  = 0x647a  (MLKEM768-X25519)
  KDF  = 0x0001  (HKDF-SHA256)
  AEAD = 0x0003  (ChaCha20Poly1305)

  suite_id = b"HPKE" || 0x647a || 0x0001 || 0x0003

Functions:

  labeled_extract(salt, label, ikm)
    labeled_ikm = b"HPKE-v1" || suite_id || label || ikm
    HKDF-Extract(salt, labeled_ikm)

  labeled_expand(prk, label, info, L)
    labeled_info = I2OSP(L, 2) || b"HPKE-v1" || suite_id || label || info
    HKDF-Expand(prk, labeled_info, L)

  key_schedule_base(shared_secret, info)
    psk_id_hash = labeled_extract("", "psk_id_hash", "")
    info_hash   = labeled_extract("", "info_hash", info)
    ks_context  = 0x00 || psk_id_hash || info_hash
    secret      = labeled_extract(shared_secret, "secret", "")
    key         = labeled_expand(secret, "key", ks_context, 32)
    base_nonce  = labeled_expand(secret, "base_nonce", ks_context, 12)

  seal_base(pk, info, aad, plaintext)
    (shared_secret, enc) = hybrid_kem::encapsulate(pk)
    (key, base_nonce)    = key_schedule_base(shared_secret, info)
    ct = ChaCha20-Poly1305(key, base_nonce, aad, plaintext)
    return (enc, ct)

  open_base(seed, enc, info, aad, ciphertext)
    shared_secret        = hybrid_kem::decapsulate(seed, enc)
    (key, base_nonce)    = key_schedule_base(shared_secret, info)
    pt = ChaCha20-Poly1305-Open(key, base_nonce, aad, ciphertext)
    return pt

### 1c: age Recipient and Identity implementations

New file: src/keys/pq.rs

PqRecipient (holds HybridPublicKey):

  impl age::Recipient for PqRecipient {
      fn wrap_file_key(&self, file_key: &FileKey)
          -> Result<(Vec<Stanza>, HashSet<String>), EncryptError>
      {
          let (enc, ct) = hpke::seal_base(
              &self.pk,
              b"age-encryption.org/mlkem768x25519",
              b"",
              file_key.expose_secret(),
          );
          let stanza = Stanza {
              tag: "mlkem768x25519".into(),
              args: vec![base64_encode(&enc)],
              body: ct,
          };
          Ok((vec![stanza], HashSet::from(["postquantum".into()])))
      }
  }

PqIdentity (holds HybridSeed, ZeroizeOnDrop):

  impl age::Identity for PqIdentity {
      fn unwrap_stanza(&self, stanza: &Stanza)
          -> Option<Result<FileKey, DecryptError>>
      {
          if stanza.tag != "mlkem768x25519" { return None; }
          // Validate: exactly 1 arg, base64 decodes to 1120 bytes
          // Validate: body is exactly 32 bytes
          let enc = base64_decode(&stanza.args[0]);
          let pt = hpke::open_base(
              &self.seed,
              &enc,
              b"age-encryption.org/mlkem768x25519",
              b"",
              &stanza.body,
          );
          Some(FileKey::new(Box::new(pt)))
      }
  }

Key encoding (matching Go age v1.3.1):
  Recipient: bech32 with HRP "age1pq"
  Identity:  bech32 with HRP "AGE-SECRET-KEY-PQ-"

  parse_pq_recipient(s: &str) -> Result<PqRecipient>
  parse_pq_identity(s: &str)  -> Result<PqIdentity>
  format_pq_recipient(pk: &HybridPublicKey) -> String
  format_pq_identity(seed: &HybridSeed)     -> String

### 1d: BIP39 PQ key derivation

Modified file: src/keys/mnemonic.rs

New constant alongside existing salts:

    const PQ_SALT: &[u8] = b"blu-pq-v1";

New functions:

    derive_pq_seed(seed: &Seed) -> HybridSeed
      Calls derive_key(seed, PQ_SALT) to get 32 bytes.
      This 32-byte value becomes the age PQ identity seed.
      expandKey() deterministically derives the full ML-KEM-768 +
      X25519 keypair from it.

    derive_pq_identity(seed: &Seed) -> PqIdentity
    derive_pq_recipient(seed: &Seed) -> PqRecipient

### 1e: KEK wrapping with PQ

Modified file: src/keys/kek.rs

Current wrap_for_recipients() hardcodes age::x25519::Recipient
parsing. Changes:

  wrap_for_recipients()
    Accept &[&dyn age::Recipient] instead of parsing X25519 strings.
    The caller provides PqRecipient instances for new wraps.
    Internally it is the same age::Encryptor::with_recipients() call.
    The age crate handles stanza generation; we just plug in our
    Recipient impl.

  unwrap_with_identity()
    Accept &[&dyn age::Identity] instead of parsing a single X25519
    identity string. Pass both PqIdentity and x25519::Identity for
    backward compat (old wrapped.age files have X25519 stanzas, new
    ones have mlkem768x25519 stanzas).

  KekVersionInfo.users
    The users field stores recipient strings. It already supports
    arbitrary strings. PQ recipients (age1pq...) are just longer
    strings. No structural change needed.

  Default behavior:
    New KEK wraps use PQ recipients.
    Old wrapped.age files with X25519 stanzas remain readable via
    the X25519 identity fallback.

Modified file: src/keys/mod.rs
  Add PQ key functions alongside existing X25519 functions.
  generate_pq_keypair(), parse_pq_recipient(), etc.

### 1f: Identity format and CLI updates

Modified file: src/cli/identity_cmd.rs

  blu identity init
    Derives both X25519 and PQ keys from the mnemonic.
    Stores both public keys in identity.toml.
    Displays both.

  blu identity show
    Shows both public keys.

  blu identity recover
    Re-derives both key types from the mnemonic.

Updated identity.toml format:

    public_key = "age1abc..."          # X25519 (backward compat)
    pq_public_key = "age1pq1xyz..."    # ML-KEM-768 + X25519 hybrid
    created = "2026-04-18T..."

### 1g: Agent state updates

Modified file: src/agent/state.rs

  AgentState gains:
    pq_seed: Option<HybridSeed>   (32 bytes, ZeroizeOnDrop)

  unlock() / unlock_with_secret()
    Derive PQ seed from the BIP39 seed alongside X25519 identity.

  load_kek()
    Provide both PqIdentity and x25519::Identity to
    unwrap_with_identity() for backward compat.

  lock()
    Zeroize the PQ seed.

No agent protocol changes needed. The agent wraps/unwraps KEKs
internally; PQ key material stays in the agent's memory.

### 1h: Integration tests and interop

  Test that our mlkem768x25519 stanzas can be decrypted by Go age
  v1.3.1 (shell out to age --decrypt).

  Test that Go age-keygen --pq keys work with our implementation
  (parse their bech32, decrypt a file they encrypted).

  Download test vectors from age-encryption.org/testkit.

  Full round-trip: BIP39 -> PQ keys -> wrap KEK -> unwrap KEK
  -> wrap DEK -> encrypt data -> decrypt data.


## Stage 2: mlock for Agent Secrets

### 2a: mlock helper module

New file: src/agent/memlock.rs

Using libc (already a dependency):

  mlock_slice(ptr, len)   wraps libc::mlock()
  munlock_slice(ptr, len) wraps libc::munlock()
  mark_dontdump(ptr, len) calls libc::madvise(MADV_DONTDUMP) on
                          Linux; no-op on macOS

Error handling: warn on failure, do not abort. Some environments
(containers, low RLIMIT_MEMLOCK) may not allow mlock.

### 2b: Apply to AgentState secrets

Modified file: src/agent/state.rs

  On unlock():
    mlock backing memory for secret_key (String buffer), kek (32
    bytes), and pq_seed (32 bytes).

  On lock():
    munlock before zeroize+drop.

  Limitation: age::x25519::Identity internal Curve25519 scalar
  cannot be mlocked (owned by the age crate). Document this.

### 2c: Platform considerations

  macOS: mlock succeeds for typical sizes (< 4KB) without elevated
  privileges. No MADV_DONTDUMP equivalent; MADV_ZERO_WIRED_PAGES
  is available but serves a different purpose.

  Linux: default RLIMIT_MEMLOCK is 64KB for unprivileged users.
  Our key material is < 4KB total. Well within limits.

  Graceful degradation: warn to stderr if mlock fails, continue.


## Stage 3: Multi-User Access

Uses the existing design from ENVELOPE_ENCRYPTION_DESIGN.md (lines
366-431), adapted for PQ recipients:

  blu user invite <age1pq...>
    Owner decrypts current KEK with their PQ identity.
    Re-wraps KEK for the invitee's PQ recipient.
    Writes .blu/invitations/<fingerprint>.age and pushes to backend.

  blu user accept --vault <uri>
    Fetches invitation from backend.
    Decrypts with own PQ identity.
    Updates wrapped KEK file to include self as recipient.
    Deletes invitation.

  blu user remove <age1pq...>
    Generates new KEK version.
    Wraps for remaining PQ recipients only.
    Marks old KEK deprecated.
    Background: re-wrap all DEKs from old KEK to new KEK.

  blu user list
    Shows authorized users from kek.toml.

All new recipients are PQ (age1pq...). The "postquantum" label in
the age Recipient trait enforces that PQ recipients are not mixed
with classical X25519 recipients in new wraps. Old X25519-wrapped
KEKs remain readable for backward compat.

Key exchange (sharing PQ public keys) is out-of-band.
The vault (S3/local) is the relay for invitations.


## Stage 4: Recovery Kit

  blu recovery-kit generate
    Displays the 24 BIP39 words.
    Optionally saves to PDF.
    The mnemonic deterministically derives both X25519 and PQ keys,
    so recovery is complete.

  blu identity recover (already exists)
    Would re-derive both key types from the mnemonic.

Recovery kit format from ENVELOPE_ENCRYPTION_DESIGN.md (lines
485-549) applies unchanged.


## Stage 5: CI/CD (low priority)

GitHub Actions for cargo build + cargo test on push.


## Dependency Summary

  Crate               Version   New?      Purpose
  ml-kem              0.2.3     New       ML-KEM-768 (FIPS 203)
  sha3                0.10      New       SHA3-256 + SHAKE256
  age                 0.11.2    Existing  File format, traits
  chacha20poly1305    0.10      Existing  HPKE AEAD + DEK wrapping
  hkdf + sha2         0.12+0.10 Existing  HPKE key schedule + HKDF
  x25519-dalek        2.x       Existing  X25519 DH (transitive)
  bech32              0.9       Existing  Key encoding
  zeroize             1.x       Existing  Secret memory wiping
  libc                0.2       Existing  mlock/munlock

No pre-release dependencies. No incompatible version bumps.
ml-kem 0.2.3 uses rand_core 0.6, compatible with the existing tree.


## Cryptographic Constants Reference

  HPKE suite ID:   b"HPKE" || 0x647a || 0x0001 || 0x0003
  KEM label:       b"\x5c\x2e\x2f\x2f\x5e\x5c"
  HPKE info:       b"age-encryption.org/mlkem768x25519"
  HPKE aad:        b""  (empty)
  File key size:   16 bytes
  Stanza body:     32 bytes (16 file key + 16 Poly1305 tag)
  enc size:        1120 bytes (1088 ML-KEM ct + 32 X25519 eph pk)
  Public key:      1216 bytes (1184 ML-KEM ek + 32 X25519 pk)
  Seed:            32 bytes (CSPRNG or HKDF-derived)
  Seed expansion:  SHAKE256(seed) -> 96 bytes (64 PQ + 32 X25519)

  Recipient HRP:   age1pq
  Identity HRP:    AGE-SECRET-KEY-PQ-

  BIP39 HKDF salt: b"blu-pq-v1"
