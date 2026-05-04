# Envelope Encryption Design for blu

Canonical design document for blu's encryption architecture. Covers
key hierarchy, envelope encryption, agent protocol, multi-user access,
key rotation, and recovery.

Implementation status (May 2026):

  Core key hierarchy (BIP39, UK, KEK, DEK)   DONE
  Agent daemon with session management        DONE
  v2 file format (header + DEK payload)       DONE
  KEK storage and wrapping                    DONE
  BIP39 identity with biometric unlock        DONE
  Post-quantum hybrid KEM (ML-KEM-768)        DONE (see PLAN-PQ.md)
  mlock for agent secrets                     DONE
  Multi-user access (invite/accept/remove)    NOT STARTED
  KEK rotation CLI                            NOT STARTED
  Recovery kit (PDF export)                   NOT STARTED

## Overview

This document describes blu's encryption architecture using envelope
encryption with BIP39-based key derivation. The design supports
multiple users, key rotation, and provides a secure recovery
mechanism.

## Goals

1. **User-friendly key management** - BIP39 mnemonic that users can memorize or write down
2. **Multi-user support** - Multiple users can access the same vault
3. **Key rotation** - Rotate encryption keys without re-encrypting all data
4. **Secure session management** - Daemon-based agent to avoid repeated passphrase entry
5. **Recovery** - PDF "recovery kit" similar to 1Password

## Key Hierarchy

```
+------------------------------------------------------------------+
|                         User Layer                                |
+------------------------------------------------------------------+
|                                                                   |
|   BIP39 Mnemonic (12/15/18/21/24 words)                          |
|         + Optional Passphrase ("25th word")                       |
|                |                                                  |
|                v                                                  |
|   +----------------------+                                        |
|   |   User Seed (512b)   |  BIP39 seed derivation                |
|   +----------------------+  (PBKDF2-HMAC-SHA512, 2048 rounds)    |
|                |                                                  |
|                v                                                  |
|   +----------------------+                                        |
|   |  User Key (UK)       |  X25519 keypair derived from seed     |
|   |  - Private (uk_priv) |  via HKDF-SHA256                      |
|   |  - Public (uk_pub)   |                                        |
|   +----------------------+                                        |
|                                                                   |
+------------------------------------------------------------------+
                              |
                              | UK encrypts KEK (via age)
                              v
+------------------------------------------------------------------+
|                         Vault Layer                               |
+------------------------------------------------------------------+
|                                                                   |
|   +---------------------------+                                   |
|   | Key Encryption Key (KEK)  |  256-bit symmetric key            |
|   +---------------------------+  One per vault, rotatable         |
|                |                                                  |
|                |  Stored as age file with multiple recipients:    |
|                |  +------------------------------------------+    |
|                |  | .blu/keys/kek_v1/wrapped.age             |    |
|                |  |                                          |    |
|                |  | age-encrypted to ALL authorized users:   |    |
|                |  |   - age1alice...                         |    |
|                |  |   - age1bob...                           |    |
|                |  |   - age1charlie...                       |    |
|                |  +------------------------------------------+    |
|                |                                                  |
+----------------|-------------------------------------------------+
                 |
                 | KEK wraps DEKs (ChaCha20-Poly1305)
                 v
+------------------------------------------------------------------+
|                         Data Layer                                |
+------------------------------------------------------------------+
|                                                                   |
|   +---------------------------+                                   |
|   | Data Encryption Key (DEK) |  256-bit symmetric key            |
|   +---------------------------+  One per blob file                |
|                |                 One per index file               |
|                |                                                  |
|                |  Stored as header in each blob/index:            |
|                |  +------------------------------------------+    |
|                |  | File Structure:                          |    |
|                |  |   [4 bytes]  Magic                       |    |
|                |  |   [2 bytes]  Format version              |    |
|                |  |   [2 bytes]  KEK version used            |    |
|                |  |   [N bytes]  Encrypted DEK (wrapped)     |    |
|                |  |   [...]      Encrypted data              |    |
|                |  +------------------------------------------+    |
|                |                                                  |
|                v                                                  |
|   +---------------------------+                                   |
|   |    Encrypted Data         |  ChaCha20-Poly1305 with DEK      |
|   +---------------------------+                                   |
|                                                                   |
+------------------------------------------------------------------+
```

## Design Decisions

| Question | Decision | Rationale |
|----------|----------|-----------|
| Vault ID | UUID, persists across restores | Simple, unique, directory-independent identity |
| KEK storage | Single file with age multi-recipient | Cleaner than per-user files, age handles it natively |
| Index encryption | Own DEK, same as blobs | Consistency, self-contained for sync |
| Agent behavior | Auto-start, explicit unlock required | Unlock once per session, not per command |
| BIP39 passphrase | Included from v1 | Design for correctness from start |
| User add flow | Async via backend (invitations) | No central server, but backend can queue |

## Components

### 1. BIP39 Mnemonic & Seed Derivation

**Mnemonic Generation:**
- Support 12, 15, 18, 21, or 24 words (128-256 bits entropy)
- Use standard BIP39 English wordlist (2048 words)
- Optional passphrase (the "25th word") for additional security

**Seed Derivation (BIP39 standard):**
```
mnemonic + "mnemonic" + passphrase -> PBKDF2-HMAC-SHA512 (2048 rounds) -> 512-bit seed
```

**User Key Derivation:**
```
seed -> HKDF-SHA256(
    ikm = seed,
    salt = "blu-x25519-v1",
    info = ""  # No vault-specific binding at UK level
) -> 32 bytes -> X25519 keypair
```

**Post-Quantum Key Derivation (added April 2026):**
```
seed -> HKDF-SHA256(
    ikm = seed,
    salt = "blu-pq-v1",
    info = ""
) -> 32 bytes -> HybridSeed -> SHAKE256 -> ML-KEM-768 + X25519 keypair
```

**Device Key Derivation (for biometric unlock):**
```
seed -> HKDF-SHA256(
    ikm = seed,
    salt = "blu-device-key-v1",
    info = ""
) -> 32 bytes -> Device Key (encrypts seed in Keychain)
```

Three distinct HKDF salts ensure domain separation between key types.
All three key types are deterministically derived from the same BIP39
seed, so recovering the mnemonic recovers everything.

Note: UK is vault-independent. The same mnemonic = same UK = same identity across all vaults. This is intentional - your identity follows you.

### 2. Key Encryption Key (KEK)

**Properties:**
- 256-bit symmetric key
- Used only for wrapping/unwrapping DEKs (never encrypts data directly)
- One active KEK per vault at a time
- Versioned for rotation (v0, v1, v2, ...)
- Wrapped using age with multiple recipients (all authorized users)

**Storage Structure:**
```
.blu/keys/
  kek.toml              # KEK metadata
  kek_v1/
    wrapped.age         # KEK encrypted to all authorized users (age multi-recipient)
  kek_v0/               # Previous version (for reading old blobs during migration)
    wrapped.age
```

**KEK Metadata (`kek.toml`):**
```toml
current_version = 1
created = "2024-01-15T10:30:00Z"

[[versions]]
version = 1
created = "2024-01-15T10:30:00Z"
status = "active"
users = ["age1alice...", "age1bob..."]

[[versions]]
version = 0
created = "2024-01-01T00:00:00Z"
status = "deprecated"
deprecated_at = "2024-01-15T10:30:00Z"
users = ["age1alice..."]
```

**Status Values:**
- `active` - Current KEK, used for new encryptions
- `deprecated` - Old KEK, kept for reading old data, not used for new encryptions
- `archived` - All data migrated away, can be deleted (future cleanup)

### 3. Data Encryption Key (DEK)

**Properties:**
- 256-bit symmetric key (ChaCha20-Poly1305)
- One per blob file
- One per index file (plain_index, blob_index, tag_index)
- Generated randomly when file is created
- Wrapped by KEK and stored in file header

**Blob File Format (v2):**
```
Offset  Size    Field
------  ------  -----
0       4       Magic: "BLUB" (0x424C5542)
4       2       Format version: 2 (little-endian)
6       2       KEK version used to wrap DEK (little-endian)
8       4       Wrapped DEK length in bytes (N) (little-endian)
12      N       Wrapped DEK (ChaCha20-Poly1305: nonce || ciphertext || tag)
12+N    ...     Encrypted data (DEK-encrypted chunks, same format as v1)
```

**Index File Format (v2):**
```
Offset  Size    Field
------  ------  -----
0       4       Magic: "BLUI" (0x424C5549)
4       2       Format version: 2 (little-endian)
6       2       KEK version used to wrap DEK (little-endian)
8       4       Wrapped DEK length in bytes (N) (little-endian)
12      N       Wrapped DEK
12+N    ...     Encrypted index data (DEK-encrypted, compressed, serialized)
```

### 4. Agent Daemon (blu-agent)

**Purpose:** 
- Keep decrypted UK and KEKs in memory
- Avoid repeated passphrase/mnemonic entry
- Provide secure channel for CLI to request crypto operations

**Architecture:**
```
+---------------+              +------------------------+
|   blu CLI     |   Unix       |     blu-agent          |
|               |   Socket     |                        |
|  (any cmd)    | -----------> |  Holds in memory:      |
|               |   JSON-RPC   |    - User private key  |
|               | <----------- |    - Decrypted KEKs    |
+---------------+              |                        |
                               |  Operations:           |
                               |    - Unlock vault      |
                               |    - Wrap DEK          |
                               |    - Unwrap DEK        |
                               |    - Lock vault        |
                               +------------------------+
                                        |
                               Socket: ~/.blu/agent.sock
                               PID:    ~/.blu/agent.pid
```

**Protocol (JSON-RPC 2.0 over Unix socket):**

```json
// Unlock agent with mnemonic (first time or after lock)
{
  "jsonrpc": "2.0",
  "method": "unlock",
  "params": {
    "mnemonic": "word1 word2 ... word24",
    "passphrase": "optional passphrase"
  },
  "id": 1
}
// Response
{
  "jsonrpc": "2.0",
  "result": {
    "public_key": "age1...",
    "expires_at": "2024-01-15T11:30:00Z"
  },
  "id": 1
}

// Unlock a specific vault (load its KEK)
{
  "jsonrpc": "2.0",
  "method": "unlock_vault",
  "params": {
    "vault_path": "/path/to/.blu"
  },
  "id": 2
}
// Response
{
  "jsonrpc": "2.0",
  "result": {
    "vault_id": "a1b2c3d4-...",
    "kek_version": 1
  },
  "id": 2
}

// Wrap a new DEK (for writing new blob/index)
{
  "jsonrpc": "2.0",
  "method": "wrap_dek",
  "params": {
    "vault_path": "/path/to/.blu"
  },
  "id": 3
}
// Response
{
  "jsonrpc": "2.0",
  "result": {
    "dek": "base64-encoded-32-bytes",
    "wrapped_dek": "base64-encoded-wrapped",
    "kek_version": 1
  },
  "id": 3
}

// Unwrap a DEK (for reading blob/index)
{
  "jsonrpc": "2.0",
  "method": "unwrap_dek",
  "params": {
    "vault_path": "/path/to/.blu",
    "wrapped_dek": "base64-encoded-wrapped",
    "kek_version": 1
  },
  "id": 4
}
// Response
{
  "jsonrpc": "2.0",
  "result": {
    "dek": "base64-encoded-32-bytes"
  },
  "id": 4
}

// Lock vault (clear KEK from memory)
{
  "jsonrpc": "2.0",
  "method": "lock_vault",
  "params": {
    "vault_path": "/path/to/.blu"
  },
  "id": 5
}

// Lock agent entirely (clear UK and all KEKs)
{
  "jsonrpc": "2.0",
  "method": "lock",
  "params": {},
  "id": 6
}

// Get agent status
{
  "jsonrpc": "2.0",
  "method": "status",
  "params": {},
  "id": 7
}
// Response
{
  "jsonrpc": "2.0",
  "result": {
    "unlocked": true,
    "public_key": "age1...",
    "expires_at": "2024-01-15T11:30:00Z",
    "vaults": [
      {"path": "/path/to/.blu", "vault_id": "a1b2c3d4-..."}
    ]
  },
  "id": 7
}
```

**Lifecycle:**
1. First `blu` command checks for agent socket
2. If not running, auto-spawns `blu-agent` as background daemon
3. Agent prompts for mnemonic (via CLI passthrough) on first vault access
4. Configurable timeout (default: 1 hour idle, or 8 hours max)
5. `blu lock` clears secrets immediately
6. `blu agent stop` terminates daemon

**Security Measures:**
- Socket permissions: 0600 (owner read/write only)
- Use `mlock()` to prevent secrets from being swapped to disk
- Zeroize all secrets on drop (using `zeroize` crate)
- Timeout-based auto-lock
- No logging of secret material

### 5. Multi-User Access

**User Identity:**

Each user has a global identity (not per-vault):
```
~/.blu/
  identity.toml       # User's public key for sharing
  identity.age        # User's encrypted private key (optional backup)
```

**identity.toml:**
```toml
public_key = "age1abc123..."
created = "2024-01-15T10:00:00Z"
```

**Adding a User (Async via Backend):**

Since we don't have a central server, we use the backend to queue invitations:

```bash
# Alice (vault owner) creates invitation for Bob
$ blu user invite age1bob...
Invitation created: .blu/invitations/age1bob.age
Syncing to backend...
Done. Bob can now run: blu user accept

# Bob (on his machine) accepts the invitation
$ blu user accept --vault s3://bucket/path
Fetching invitation...
Decrypting KEK...
You now have access to vault a1b2c3d4-...
```

**What Happens During Invite:**
1. Alice's CLI talks to agent to decrypt current KEK
2. CLI re-encrypts KEK using age with Bob's public key
3. Writes to `.blu/invitations/age1bob.age`
4. Pushes to backend

**What Happens During Accept:**
1. Bob's CLI fetches `.blu/invitations/age1bob.age` from backend
2. Bob's agent decrypts it using his UK
3. Bob now has the KEK
4. CLI updates `.blu/keys/kek_vN/wrapped.age` to include Bob as a recipient
5. Deletes the invitation file

**Removing a User:**

```bash
$ blu user remove age1bob...
Removing Bob from authorized users...
Rotating KEK (Bob had access to v1)...
New KEK version: v2
Re-wrapping DEKs in background...
Done.
```

**What Happens:**
1. Generate new KEK (v2)
2. Wrap v2 for all remaining users (not Bob)
3. Mark v1 as deprecated
4. Background task: re-wrap all DEKs from v1 to v2
5. Once complete, mark v1 as archived

### 6. KEK Rotation

**Triggers:**
- Manual: `blu kek rotate`
- User removal: automatic (security requirement)
- Scheduled: configurable (default: 90 days, 0 = disabled)

**Rotation Process:**

```
1. Generate new KEK (v_new)
        |
        v
2. Wrap v_new for all current authorized users
        |
        v
3. Write .blu/keys/kek_v{new}/wrapped.age
        |
        v
4. Update kek.toml: v_new = active, v_old = deprecated
        |
        v
5. Background: For each blob/index file:
   a. Read file header
   b. If kek_version < v_new:
      - Unwrap DEK with old KEK
      - Re-wrap DEK with new KEK
      - Rewrite file header (data unchanged!)
        |
        v
6. Once all files migrated: mark v_old = archived
```

**Key Insight:** Rotation only re-wraps DEKs (tiny), not re-encrypts data (huge). A vault with 1TB of data might have ~125,000 blob files. Re-wrapping 125k DEKs is fast; re-encrypting 1TB is not.

**Background Migration:**

```bash
$ blu kek rotate
Generating new KEK (v2)...
Wrapping for 3 users...
KEK v2 is now active.

Background migration started.
Run 'blu kek status' to check progress.

$ blu kek status
Current KEK: v2 (active)
Previous KEK: v1 (deprecated)
  Migration progress: 45,231 / 125,000 files (36%)
  Estimated time remaining: 12 minutes
```

### 7. Recovery Kit

**Purpose:** Allow user to recover access if they lose their device.

**Contents:**
- BIP39 mnemonic words
- Vault ID (for identification, not security)
- Instructions

**Generation:**

```bash
$ blu recovery-kit generate
WARNING: This will display your secret recovery phrase.
Anyone with this phrase can access ALL your vaults.

Continue? [y/N] y

+------------------------------------------------------------+
|                    BLU RECOVERY KIT                        |
|                                                            |
|  Created: 2024-01-15                                       |
|  User: age1abc123...                                       |
|                                                            |
|  ----------------------------------------------------      |
|                                                            |
|  RECOVERY PHRASE (24 words):                               |
|                                                            |
|    1. abandon      9. idea       17. solar                 |
|    2. ability     10. jazz       18. table                 |
|    3. able        11. kite       19. tree                  |
|    4. about       12. lamp       20. uncle                 |
|    5. above       13. oak        21. valve                 |
|    6. absent      14. piano      22. water                 |
|    7. guitar      15. queen      23. xbox                  |
|    8. hero        16. rain       24. yard                  |
|                                                            |
|  PASSPHRASE: ********** (if set)                           |
|                                                            |
|  ----------------------------------------------------      |
|                                                            |
|  TO RECOVER:                                               |
|    1. Install blu on your new device                       |
|    2. Run: blu identity recover                            |
|    3. Enter your 24 words (and passphrase if set)          |
|    4. Your identity is restored                            |
|                                                            |
|  WARNING: Store this securely! Anyone with these           |
|  words can access your encrypted data.                     |
|                                                            |
+------------------------------------------------------------+

Save to PDF? [Y/n] y
Saved to: blu-recovery-kit-2024-01-15.pdf
```

**Recovery Process:**

```bash
$ blu identity recover
Enter your recovery phrase (24 words):
> abandon ability able about above absent guitar hero idea jazz kite lamp oak piano queen rain solar table tree uncle valve water xbox yard

Enter passphrase (or press Enter for none):
> ********

Deriving keys...
Identity recovered!
Public key: age1abc123...

Your identity is now active. You can access any vaults
where this identity was authorized.
```

## File Structure

Run `find .blu -type f` or `tree .blu` to see the current layout.
Key directories and files:

### Per-Vault (`.blu/`)

`.blu/config.toml` contains backend config, encryption settings
(recipient, pq_recipient, identity_file path).

`.blu/identity.age` is the vault's local copy of the user's age
identity (optionally passphrase-encrypted).

`.blu/keys/` contains the KEK store: `kek.toml` (metadata with
version history and authorized users) and `kek_vN/wrapped.age`
directories (one per KEK version, each containing the KEK encrypted
via age to authorized recipients).

`.blu/invitations/` (future, not yet implemented) will hold pending
multi-user invitations.

`.blu/indexes/` contains encrypted index files (index.dat,
blob_index.dat, tag_index.dat) in v2 format with per-file DEKs.

`.blu/data/` contains encrypted blob files in v2 format, organized
by content hash prefix (e.g. `a/ab/abcd...`).

### Per-User (`~/.blu/`)

`~/.blu/identity.toml` holds public keys (X25519 and PQ) and
creation metadata. Safe to share.

`~/.blu/identity.age` is the age identity file (private key),
optionally passphrase-encrypted.

`~/.blu/identity.enc` (when biometric is configured) holds the
BIP39 seed encrypted with the device key, stored in the macOS
Keychain with biometric access policy.

`~/.blu/agent.sock` and `~/.blu/agent.pid` are the agent daemon's
Unix socket and PID file.

`~/.blu/config.toml` holds user preferences (timeout profile,
auto-start settings).

**vault.toml:**
```toml
vault_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
created = "2024-01-15T10:00:00Z"
blu_version = "0.5.0"
```

**~/.blu/config.toml:**
```toml
[agent]
timeout_idle = "1h"      # Lock after 1 hour of inactivity
timeout_max = "8h"       # Lock after 8 hours regardless
auto_start = true        # Auto-start agent on first command

[defaults]
mnemonic_words = 24      # Default word count for new identities
```

## CLI Commands

### Identity Management

```bash
# Generate new identity (creates mnemonic)
blu identity init [--words 12|15|18|21|24]

# Show public key
blu identity show

# Recover identity from mnemonic
blu identity recover
```

### Vault Operations

```bash
# Initialize new vault (uses current identity)
blu init <path> [--backend local|s3] [--bucket <name>] [--region <region>]

# Standard operations (unchanged, but now use agent)
blu sync [--push]
blu ls
blu restore-files [--all] [--path <pattern>] [--to <dir>]
blu pull [--force]
blu status
```

### User Management

```bash
# List users with access to vault
blu user list

# Invite a user (async)
blu user invite <public-key>

# Accept an invitation
blu user accept --vault <path-or-uri>

# Remove a user (triggers KEK rotation)
blu user remove <public-key>
```

### Key Management

```bash
# Show KEK status
blu kek status

# Manual rotation
blu kek rotate

# Configure auto-rotation
blu kek set-schedule <duration>   # e.g., "90d", "0" to disable
```

### Agent Management

```bash
# Show agent status
blu agent status

# Stop agent daemon
blu agent stop

# Lock current vault
blu lock

# Lock all vaults and agent
blu lock --all

# Unlock (prompts for mnemonic if needed)
blu unlock
```

### Recovery

```bash
# Generate recovery kit
blu recovery-kit generate [--output <file.pdf>]
```

## Cryptographic Specifications

| Purpose | Algorithm | Parameters |
|---------|-----------|------------|
| Mnemonic entropy | CSPRNG | 128/160/192/224/256 bits |
| Mnemonic to seed | PBKDF2-HMAC-SHA512 | 2048 rounds, salt="mnemonic"+passphrase |
| Seed to UK | HKDF-SHA256 | salt="blu-user-key-v1", info="" |
| User Key | X25519 | 32-byte private, 32-byte public |
| KEK | Random | 256 bits |
| KEK wrapping | age | X25519-based, multi-recipient |
| DEK | Random | 256 bits |
| DEK wrapping | ChaCha20-Poly1305 | 12-byte nonce, 16-byte tag |
| Data encryption | ChaCha20-Poly1305 | 12-byte nonce, 16-byte tag |

## Security Considerations

### Threat Model

**Protected against:**
- Cloud provider reading data (all data encrypted client-side)
- Stolen device without agent running (secrets not in memory)
- Compromised single DEK (only one blob exposed)
- Compromised old KEK after rotation (new data uses new KEK)
- Removed user accessing new data (KEK rotated on removal)

**Not protected against:**
- Compromised device while agent is unlocked (attacker has UK)
- Rubber hose cryptanalysis (user reveals mnemonic under duress)
- Compromised mnemonic (full access to all user's vaults)
- Quantum computers targeting the X25519 UK->KEK layer (mitigated
  by PQ hybrid KEM; see PLAN-PQ.md)

### Implementation Requirements

1. **Memory safety:** Use `zeroize` crate for all secret types
2. **Memory locking:** Use `mlock()` for agent's secret storage
3. **Secure random:** Use `getrandom` or OS CSPRNG
4. **Constant-time comparison:** For all secret comparisons
5. **No secret logging:** Never log keys, mnemonics, or DEKs

## Future Considerations

### Post-Quantum Cryptography (DONE)

Implemented April 2026 via ML-KEM-768 + X25519 hybrid KEM. See
PLAN-PQ.md for full details. Summary:

- UK->KEK: New vaults wrap KEK using mlkem768x25519 (HPKE,
  spec-compliant with C2SP age v1.1.0). Interoperable with Go
  age v1.3.1.
- KEK->DEK: ChaCha20-Poly1305 with 256-bit keys (quantum-safe).
- DEK->data: ChaCha20-Poly1305 with 256-bit keys (quantum-safe).
- BIP39 seed derives PQ keys via separate HKDF path ("blu-pq-v1").
- Agent receives PQ seed via biometric unlock path.
- Passphrase-only unlock path is X25519-only (PQ KEKs require
  biometric or mnemonic recovery). This is an age spec constraint:
  PQ and classical recipients cannot be mixed in one age file.

### Hardware Key Support

Design allows future integration:
- UK could be stored on YubiKey/Ledger
- Agent would communicate with hardware device
- Mnemonic backup would still work as recovery

### Vault Sharing via URL

Future feature: Generate shareable vault URLs
```
blu://vault/s3:bucket:prefix?invite=age1bob...
```

## Appendix A: BIP39 Word Counts

| Words | Entropy (bits) | Security Level |
|-------|----------------|----------------|
| 12 | 128 | Standard |
| 15 | 160 | Enhanced |
| 18 | 192 | High |
| 21 | 224 | Very High |
| 24 | 256 | Maximum |

Recommendation: Default to 24 words. Storage cost is minimal, security benefit is significant.

## Appendix B: Example Flows

### New User, New Vault

```
1. User runs: blu identity init --words 24
2. Agent starts, generates mnemonic, derives UK
3. User shown recovery phrase, prompted to save it
4. identity.toml written to ~/.blu/

5. User runs: blu init /data/photos --backend s3 --bucket my-bucket
6. CLI generates vault_id (UUID)
7. CLI generates KEK (v0)
8. CLI wraps KEK for user's UK
9. CLI writes vault.toml, config.toml, kek.toml, wrapped.age
10. Vault ready for use
```

### Adding Second User

```
1. Bob runs: blu identity init (on his machine)
2. Bob shares his public key: age1bob...

3. Alice runs: blu user invite age1bob...
4. Alice's agent decrypts KEK
5. CLI wraps KEK for Bob's public key
6. CLI writes .blu/invitations/age1bob.age
7. CLI syncs to backend

8. Bob runs: blu user accept --vault s3://my-bucket/
9. Bob's CLI fetches invitation from backend
10. Bob's agent decrypts KEK using his UK
11. CLI updates wrapped.age to include Bob as recipient
12. CLI deletes invitation
13. Bob now has access
```

### KEK Rotation After User Removal

```
1. Alice runs: blu user remove age1bob...
2. CLI generates new KEK (v1)
3. CLI wraps v1 for Alice only (not Bob)
4. CLI writes kek_v1/wrapped.age
5. CLI updates kek.toml: v1=active, v0=deprecated
6. Background process starts:
   - For each file with kek_version=0:
     - Unwrap DEK with v0
     - Re-wrap DEK with v1
     - Update file header
7. Once all files migrated, v0 marked archived
8. Bob can no longer decrypt any data (old or new)
```
