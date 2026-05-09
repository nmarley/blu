# PQ Identity Design

Why the passphrase unlock path must store the post-quantum hybrid
seed, and how it fits the BIP32/BIP39 "store the derived key, not
the seed" principle.

Written May 2026 during the full PQ migration.


## Background: The Key Derivation Tree

blu uses BIP39 mnemonic phrases as the root of trust. A 24-word
mnemonic and an optional passphrase ("25th word") produce a 512-bit
seed via PBKDF2-HMAC-SHA512 (2048 rounds). Multiple purpose-specific
keys are derived from this seed using HKDF-SHA256 with distinct salts:

    Mnemonic + passphrase -> Seed (64 bytes)
      -> HKDF("blu-x25519-v1")    -> 32 bytes -> standalone X25519 (AGE-SECRET-KEY)
      -> HKDF("blu-pq-v1")        -> 32 bytes -> PQ hybrid seed (HybridSeed)
      -> HKDF("blu-device-key-v1") -> 32 bytes -> device key (biometric)

The PQ hybrid seed is further expanded via SHAKE256 into 96 bytes for
the ML-KEM-768 + X25519 keypair (the X-Wing construction per C2SP age
spec v1.1.0):

    HybridSeed (32 bytes)
      -> SHAKE256 -> 96 bytes
        [0..64]   (d, z) for ML-KEM-768 deterministic keygen
        [64..96]  X25519 private scalar

The X25519 component inside the hybrid is different from the standalone
X25519 key. They are derived from the same root seed but through
different HKDF salts, so they are cryptographically independent.


## The Wallet Analogy

In a BIP32/BIP39 cryptocurrency wallet:

    Mnemonic (24 words) -> Seed (64 bytes) -> Master Key -> derived keys
                           ^                  ^
                           NOT stored         THIS is stored in the hot wallet

The wallet stores the derived key needed for day-to-day operations.
The seed and mnemonic are backup/recovery material only. You never
keep the seed in the hot path. This limits exposure: if the hot wallet
is compromised, only derived keys are leaked. The mnemonic (which can
derive everything, including keys for other systems) remains safe
offline.


## How This Applies to blu

blu has two unlock paths that grant the agent access to key material:

1. Biometric (Touch ID): recovers the full BIP39 Seed from
   `identity.enc` (encrypted with a device key held in the macOS
   Keychain), then derives both the X25519 identity and the PQ seed.

2. Passphrase: decrypts `identity.age` with age scrypt to obtain a
   stored derived key, which the agent uses for KEK unwrapping.

The biometric path stores the full BIP39 Seed because it is protected
by hardware (Secure Enclave via Keychain). The passphrase path should
store only the derived key needed for operations, following the wallet
principle.


## The Problem (Before This Change)

`identity.age` stored the standalone X25519 private key
(`AGE-SECRET-KEY-...`). This was correct when the KEK was wrapped to
X25519 recipients. After the v1 removal (commit 5a9ff8e), all vaults
wrap the KEK exclusively to the PQ hybrid recipient (`age1pq...`).

The passphrase unlock path loads `identity.age`, obtains the X25519
key, and sends it to the agent. The agent then tries to unwrap the
KEK using only the X25519 identity. But the KEK was wrapped to the PQ
recipient, so decryption fails: "No matching keys found."

The biometric path works because it recovers the full BIP39 Seed,
derives the PQ seed, and sends it alongside the X25519 key. The agent
uses the PQ seed to build a `PqIdentity` that can unwrap the KEK.

In short: `identity.age` stores the wrong derived key. It stores the
X25519 key, but operations require the PQ hybrid seed.


## The Fix

Store the 32-byte PQ hybrid seed in `identity.age` instead of the
X25519 key. The PQ hybrid seed is the derived "master key" for the
post-quantum world; from it, SHAKE256 expands to the full ML-KEM-768
+ X25519 keypair needed to unwrap PQ-wrapped KEKs.

This follows the same wallet principle:

    identity.age (before):  AGE-SECRET-KEY-...  (X25519, wrong derived key)
    identity.age (after):   PQ hybrid seed      (32 bytes, correct derived key)

The mnemonic and BIP39 Seed remain offline backup material. The device
key remains in the Keychain. Only the PQ hybrid seed sits in the
passphrase-protected file, which is the minimum secret needed for
day-to-day operations.


## What Lives Where (After This Change)

    Storage             Contents                    Purpose
    --
    Mnemonic (paper)    24 words                    Recovery only, never on disk
    identity.enc        BIP39 Seed (64 bytes)       Biometric fast-unlock (Touch ID)
                        encrypted w/ device key     Derives everything
    identity.age        PQ hybrid seed (32 bytes)   Passphrase unlock
                        passphrase-encrypted        Derives PQ keypair for KEK unwrap
    identity.toml       Public keys, metadata       Public info only, safe to share

Both unlock paths converge at the same point: the agent ends up with
the PQ hybrid seed in memory, enabling KEK unwrapping.


## Agent Unlock Flow (After This Change)

Passphrase path:

    1. CLI prompts for passphrase
    2. Agent reads ~/.blu/identity.age
    3. Agent decrypts with age scrypt -> 32-byte PQ hybrid seed
    4. Agent stores PQ seed in state (mlocked, zeroized on lock)
    5. Agent derives X25519 identity from the PQ seed's X25519
       component (bytes [64..96] of the SHAKE256 expansion) for
       backward-compatible public key display
    6. On first vault operation, agent uses PqIdentity to unwrap KEK

Biometric path (unchanged):

    1. CLI retrieves device key from Keychain (Touch ID)
    2. CLI decrypts identity.enc -> 64-byte BIP39 Seed
    3. CLI derives X25519 identity + PQ seed from the Seed
    4. CLI sends both to agent via unlock_with_secret_pq RPC
    5. Agent stores PQ seed in state
    6. On first vault operation, agent uses PqIdentity to unwrap KEK


## Security Properties

The PQ hybrid seed (32 bytes) is strictly less sensitive than the
BIP39 Seed (64 bytes). Compromising the PQ seed exposes:

  - The ML-KEM-768 + X25519 hybrid keypair (can unwrap KEKs)
  - All data in vaults where this key is an authorized recipient

It does NOT expose:

  - The standalone X25519 identity (different HKDF salt)
  - The device key (different HKDF salt)
  - The BIP39 Seed itself (HKDF is not reversible)
  - Other derived keys from other HKDF paths

This is the minimum blast radius for the passphrase-protected file.


## Identity File Format

The new `identity.age` file contains the 32-byte PQ hybrid seed
encoded as a bech32 string with the HRP `BLU-PQ-SEED-1`. When
passphrase-protected, the bech32 string is encrypted using age's
scrypt recipient (same mechanism as before, just different payload).

Detection of old vs new format is straightforward:

  - Old format: decrypted content starts with `AGE-SECRET-KEY-`
  - New format: decrypted content starts with `BLU-PQ-SEED-1`

Since this is a greenfield deployment with no legacy identity files to
migrate, the old format can simply produce an error with guidance to
run `blu identity init`.
