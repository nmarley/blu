# blu Session Management and Envelope Encryption Design

## Summary

This document describes the redesign of blu's key management and
session handling. The goal is to eliminate repeated passphrase entry
(type it once, use it all session) and lay the cryptographic foundation
for multi-user access, key rotation, and future post-quantum
algorithms.

Three stages, each independently shippable:

1. **Agent and unlock/lock** -- fixes the immediate UX problem
2. **Envelope encryption (KEK/DEK)** -- enables key rotation and
   multi-user without re-encrypting data
3. **BIP39 identity and biometric unlock** -- 1Password-like experience

## Design Decisions

These decisions were made deliberately and should not be revisited
without good reason.

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Session mechanism | Daemon over Unix socket | Same approach as ssh-agent, gpg-agent, 1Password. Key material lives in one process with mlock(). No temp files, no env vars, no keychain abuse. |
| Agent protocol | JSON-RPC 2.0 (length-prefixed) | The agent socket is a public interface. Third-party clients (GUI, other languages) need a standard protocol. JSON-RPC gives defined error codes, batch support, and a schema others already know. |
| Binary architecture | Single binary, subcommand | `blu agent start` / `blu lock` / `blu unlock`. Simpler distribution (one artifact). The agent is not useful without `blu`. |
| Auto-start | Yes, with explicit unlock/lock available | Any command needing keys auto-starts the agent and prompts if locked. `blu unlock` exists for pre-unlocking in scripts. `blu lock` is always explicit. |
| Lock granularity | Global unlock, per-vault KEK caching | One passphrase entry unlocks the user identity. Each vault's KEK is decrypted and cached transparently on first access. `blu lock` clears everything. |
| Crypto: UK to KEK | age (asymmetric, swappable) | Age handles multi-recipient natively. Swap to ML-KEM when PQ algorithms mature. |
| Crypto: KEK to DEK | ChaCha20-Poly1305 (symmetric) | Already quantum-resistant. No dependency on asymmetric crypto at this layer. |
| Crypto: DEK to data | ChaCha20-Poly1305 (symmetric) | Same as above. Consistent symmetric layer. |
| Key derivation | 24-word BIP39 seed phrase | Algorithm-agnostic root of trust. Same mnemonic derives X25519 keys today, ML-KEM keys tomorrow, via separate HKDF paths. |
| Timeouts | Named profiles with adaptive defaults | "paranoid" (5m/1h), "balanced" (1h/8h), "relaxed" (4h/12h). Auto-adjusts to paranoid when biometric is available since re-unlock is instant. |
| Biometric | Designed for, implemented in stage 3 | Touch ID on macOS via Keychain. Device key stored in Keychain with biometric access policy. Falls back to passphrase/mnemonic. |

## Stage 1: Agent and Unlock/Lock

### Goal

Type your passphrase once per session. Every subsequent `blu` command
reuses the cached decrypted key without prompting.

### Architecture

```
blu CLI (short-lived)          blu agent (long-lived daemon)
+------------------+           +---------------------------+
| blu sync         |           |  In memory (mlock'd):     |
|                  |  Unix     |   - Decrypted age         |
|  "need BlackBox" |  socket   |     Identity (X25519)     |
|  ------------->  |  -------> |   - Cached KEKs (stage 2) |
|  <-------------  |  <------- |                           |
|  "here's the     |  JSON-RPC |  Lifecycle:               |
|   encrypted      |  2.0      |   - Auto-start on demand  |
|   result"        |           |   - Auto-lock on timeout  |
+------------------+           |   - Explicit lock/unlock  |
                               +---------------------------+
                                        |
                               Socket: ~/.blu/agent.sock
                               PID:    ~/.blu/agent.pid
```

### Agent Process

The agent is started via `blu __agent-daemon` (internal subcommand,
not user-facing). It forks and daemonizes from the same binary.

User-facing commands:

| Command | Behavior |
|---------|----------|
| `blu unlock` | Start agent if needed, prompt for passphrase, send to agent |
| `blu lock` | Tell agent to zeroize all secrets |
| `blu agent status` | Show agent state (locked/unlocked, timeout remaining, vaults) |
| `blu agent stop` | Zeroize + terminate agent process |

### Auto-Start Flow

Any command needing keys (sync, ls, restore, etc.) follows this flow:

```
1. Check for ~/.blu/agent.sock
2. If socket not found:
   a. Fork + daemonize (blu __agent-daemon)
   b. Wait for socket to appear (with timeout)
3. Connect to socket
4. Send "status" RPC
5. If agent is locked:
   a. Prompt user for passphrase (in the CLI process, via rpassword)
   b. Send "unlock" RPC with passphrase + identity file path
   c. Agent decrypts identity file, stores identity in mlock'd memory
   d. Agent zeroizes the received passphrase immediately
6. Proceed with the actual operation (encrypt/decrypt via agent RPCs)
```

### Agent Protocol

Wire framing: `[4-byte big-endian u32 length][JSON payload]`

The JSON payload follows JSON-RPC 2.0. All methods:

#### status

```json
// Request
{ "jsonrpc": "2.0", "method": "status", "params": {}, "id": 1 }

// Response
{
  "jsonrpc": "2.0",
  "result": {
    "unlocked": true,
    "public_key": "age1...",
    "expires_at": "2026-03-07T20:00:00Z",
    "vaults": []
  },
  "id": 1
}
```

#### unlock

```json
// Request
{
  "jsonrpc": "2.0", "method": "unlock",
  "params": {
    "passphrase": "...",
    "identity_path": "/path/to/.blu/identity.age"
  },
  "id": 2
}

// Response
{
  "jsonrpc": "2.0",
  "result": { "public_key": "age1...", "expires_at": "2026-03-07T20:00:00Z" },
  "id": 2
}
```

#### lock

```json
{ "jsonrpc": "2.0", "method": "lock", "params": {}, "id": 3 }
// Response: { "jsonrpc": "2.0", "result": {}, "id": 3 }
```

#### encrypt

```json
{
  "jsonrpc": "2.0", "method": "encrypt",
  "params": { "data": "<base64>", "recipient": "age1..." },
  "id": 4
}
// Response: { "jsonrpc": "2.0", "result": { "ciphertext": "<base64>" }, "id": 4 }
```

#### decrypt

```json
{
  "jsonrpc": "2.0", "method": "decrypt",
  "params": { "data": "<base64>" },
  "id": 5
}
// Response: { "jsonrpc": "2.0", "result": { "plaintext": "<base64>" }, "id": 5 }
```

#### shutdown

```json
{ "jsonrpc": "2.0", "method": "shutdown", "params": {}, "id": 6 }
// Response: { "jsonrpc": "2.0", "result": {}, "id": 6 }
```

The decrypted private key never leaves the agent process. The CLI
sends data to be encrypted/decrypted; the agent performs the operation
and returns the result. This mirrors how ssh-agent and gpg-agent work.

Trade-off: all data flows through the Unix socket. But blu already
chunks data (default 64KB), and Unix domain sockets push ~4 GB/s on
modern hardware. The bottleneck is age encryption itself, not the
socket.

### Timeout Profiles

Configurable in `~/.blu/config.toml`:

```toml
[agent]
# Named profile: "paranoid", "balanced", "relaxed", "custom"
profile = "balanced"

# Custom values (used when profile = "custom", or to override)
timeout_idle = "1h"
timeout_max = "8h"

# Auto-start agent when a command needs keys
auto_start = true
```

| Profile | Idle | Max | Intended for |
|---------|------|-----|--------------|
| paranoid | 5m | 1h | Short sessions, high security. Best paired with biometric. |
| balanced | 1h | 8h | Normal daily use. |
| relaxed | 4h | 12h | Long work sessions, lower threat environment. |

Default: `balanced`. When biometric unlock is enabled (stage 3), the
default auto-adjusts to `paranoid` since re-unlock via Touch ID is
instant. The user can override this.

### Security Measures

- `mlock()` on all buffers holding key material (prevents swap to disk)
- `zeroize` crate on all secret types (zeroes memory on drop)
- Socket permissions: `0600` (owner-only)
- PID file with advisory lock to prevent duplicate agents
- No logging of secret material (passphrase, private keys, plaintext)
- Agent refuses to run as root

### Changes to Existing Code

The key change is in `src/cli/helpers.rs`. Currently,
`load_blackbox_from_config` constructs a `BlackBox` in-process. After
stage 1, it connects to the agent and returns an `AgentBlackBox` that
delegates encrypt/decrypt to the agent over the socket.

```
// Before (current):
helpers::load_blackbox_from_config(cfg, opts)
  -> prompts for passphrase
  -> loads identity file from disk
  -> constructs BlackBox holding private key in CLI process memory

// After (stage 1):
helpers::load_blackbox_from_config(cfg, opts)
  -> connects to agent (auto-starts if needed)
  -> if locked, prompts for passphrase, sends unlock RPC
  -> returns AgentBlackBox (encrypt/decrypt delegate to agent)
```

The `BlackBox` interface (encrypt/decrypt methods) stays the same from
the perspective of all other code. sync, restore, ls, etc. call
`bbox.encrypt()` / `bbox.decrypt()` without knowing whether it is
in-process or agent-backed.

This is a clean seam. Every existing command continues to work. The
only modification is in `helpers.rs` and the new agent module.

## Stage 2: Envelope Encryption (KEK/DEK)

### Goal

Separate the key hierarchy so data encryption keys are independent
from the user's identity. This enables key rotation without
re-encrypting data, and multi-user access.

### Key Hierarchy

```
User Key (UK)                    from passphrase/mnemonic (stage 3)
   |                                currently: age X25519 identity
   | age encryption (asymmetric, swappable to PQ later)
   v
Key Encryption Key (KEK)         256-bit, one per vault, rotatable
   |
   | ChaCha20-Poly1305 (symmetric, already PQ-safe)
   v
Data Encryption Key (DEK)        256-bit, one per blob/index file
   |
   | ChaCha20-Poly1305 (symmetric, already PQ-safe)
   v
Encrypted Data
```

Asymmetric crypto (age/X25519) is used only at the top layer (UK wraps
KEK). Everything below is symmetric. This means:

- When PQ algorithms arrive, only the UK->KEK layer changes
- KEK rotation re-wraps DEKs (tiny, fast); never re-encrypts data
- The symmetric layer is already quantum-resistant (256-bit keys)

### File Format v2

#### Blob file

```
Offset   Size     Field
0        4        Magic: "BLUB" (0x424C5542)
4        2        Format version: 2 (LE u16)
6        2        KEK version (LE u16)
8        4        Wrapped DEK length N (LE u32)
12       N        Wrapped DEK (nonce || ciphertext || tag)
12+N     ...      DEK-encrypted data (compressed chunks)
```

#### Index file

Same header structure, magic "BLUI" (0x424C5549).

#### Backward compatibility

Files without a magic header are v1. Fall back to current age-based
decryption. This provides backward compatibility during migration.

### KEK Storage

```
.blu/keys/
  kek.toml              metadata (current version, authorized users)
  kek_v0/
    wrapped.age         KEK encrypted to all authorized users via age
```

`kek.toml`:

```toml
current_version = 0
created = "2026-03-07T12:00:00Z"

[[versions]]
version = 0
created = "2026-03-07T12:00:00Z"
status = "active"
users = ["age1alice..."]
```

Status values:
- `active` -- current KEK, used for new encryptions
- `deprecated` -- old KEK, kept for reading old data only
- `archived` -- all data migrated away, can be deleted

### Agent Changes for Stage 2

New RPC methods:

#### wrap_dek

Generates a new random DEK, wraps it with the vault's KEK, returns
both.

```json
{
  "jsonrpc": "2.0", "method": "wrap_dek",
  "params": { "vault_path": "/path/to/.blu" },
  "id": 10
}
// Response
{
  "jsonrpc": "2.0",
  "result": {
    "dek": "<base64-32-bytes>",
    "wrapped_dek": "<base64>",
    "kek_version": 0
  },
  "id": 10
}
```

#### unwrap_dek

Unwraps a DEK using the vault's cached KEK.

```json
{
  "jsonrpc": "2.0", "method": "unwrap_dek",
  "params": {
    "vault_path": "/path/to/.blu",
    "wrapped_dek": "<base64>",
    "kek_version": 0
  },
  "id": 11
}
// Response
{
  "jsonrpc": "2.0",
  "result": { "dek": "<base64-32-bytes>" },
  "id": 11
}
```

On first access to a vault, the agent decrypts that vault's KEK (using
the user's UK) and caches it. Subsequent wrap/unwrap operations use the
cached KEK.

### Data Flow After Stage 2

**Writing (new blob/index):**

```
1. CLI requests wrap_dek from agent (returns DEK + wrapped DEK)
2. CLI writes file header (magic, version, KEK version, wrapped DEK)
3. CLI encrypts data with DEK using ChaCha20-Poly1305 (in-process)
4. CLI writes encrypted data after header
```

**Reading (existing blob/index):**

```
1. CLI reads file header (extracts wrapped DEK + KEK version)
2. CLI sends unwrap_dek to agent (returns plaintext DEK)
3. CLI decrypts data with DEK using ChaCha20-Poly1305 (in-process)
```

This is more efficient than stage 1's approach. Only the DEK (32 bytes
wrapped) goes through the socket. Data encryption/decryption happens
in the CLI process using the symmetric DEK.

### KEK Rotation

Triggers:
- Manual: `blu kek rotate`
- User removal: automatic (security requirement)
- Scheduled: configurable (default: disabled)

Process:

```
1. Generate new KEK (v_new)
2. Wrap v_new for all current authorized users (age multi-recipient)
3. Write .blu/keys/kek_v{new}/wrapped.age
4. Update kek.toml: v_new = active, v_old = deprecated
5. Background: for each blob/index file:
   a. Read file header
   b. If kek_version < v_new:
      - Unwrap DEK with old KEK
      - Re-wrap DEK with new KEK
      - Rewrite file header only (data is unchanged)
6. Once all files migrated: mark v_old = archived
```

Key insight: rotation only re-wraps DEKs (tiny), never re-encrypts
data (huge). A vault with 1TB of data might have ~125,000 blob files.
Re-wrapping 125k DEKs is fast. Re-encrypting 1TB is not.

### Multi-User Access

Adding a user:

```
1. Alice runs: blu user invite age1bob...
2. Alice's agent decrypts current KEK
3. CLI re-encrypts KEK using age with Bob added as recipient
4. Writes to .blu/invitations/age1bob.age, pushes to backend
5. Bob runs: blu user accept --vault s3://bucket/path
6. Bob's agent decrypts invitation using his UK
7. Bob now has the KEK
```

Removing a user:

```
1. Alice runs: blu user remove age1bob...
2. Generate new KEK (v_new), wrap for remaining users only
3. Mark old KEK deprecated
4. Background: re-wrap all DEKs from old KEK to new KEK
5. Bob can no longer decrypt anything
```

### Migration (v1 to v2)

Old vaults continue to work via the agent's encrypt/decrypt RPCs from
stage 1. The `blu migrate` command:

1. Generates a KEK for the vault
2. For each index file: re-encrypt with v2 format (DEK + header)
3. For each blob file: re-encrypt with v2 format (DEK + header)

Since the data payload itself uses the same symmetric encryption, only
headers change. The migration rewrites files but the actual data
encryption is the same underlying operation.

## Stage 3: BIP39 Identity and Biometric Unlock

### Goal

Replace the current age identity file with a mnemonic-derived key. Add
Touch ID as a fast re-unlock method.

### BIP39 Key Derivation

```
24 words + optional passphrase ("25th word")
   |
   v
PBKDF2-HMAC-SHA512 (2048 rounds, salt = "mnemonic" + passphrase)
   |
   v
512-bit seed
   |
   +--> HKDF-SHA256(salt="blu-x25519-v1", info="") -> 32 bytes -> X25519 keypair
   |
   +--> HKDF-SHA256(salt="blu-ml-kem-v1", info="") -> (future PQ key)
   |
   +--> HKDF-SHA256(salt="blu-device-key-v1", info="") -> device encryption key
```

The seed is the root of trust. Algorithm-specific keys are derived via
HKDF with distinct salts. Adding a new algorithm means adding a new
derivation path. The mnemonic stays the same.

### Identity Storage

```
~/.blu/
  identity.toml          public key, created date (safe to share)
  identity.enc           seed encrypted with device key (for biometric)
  config.toml            agent config, timeout profile
```

The mnemonic is never stored on disk. The user must remember it or use
the recovery kit. `identity.enc` stores the seed encrypted with a
device key that lives in the OS keychain, gated by biometric
authentication.

### Biometric Unlock

#### Setup (first time, after `blu identity init`)

```
1. User creates identity (enters mnemonic, derives seed)
2. Agent generates random "device key" (256-bit)
3. Agent encrypts seed with device key -> writes ~/.blu/identity.enc
4. Agent stores device key in macOS Keychain with:
   - Access control: kSecAccessControlBiometryCurrentSet
   - (requires Touch ID to read the keychain item)
5. Biometric unlock is now available
```

#### Subsequent unlock (after lock or timeout)

```
1. CLI connects to agent, agent is locked
2. CLI detects ~/.blu/identity.enc exists
3. CLI asks: "Unlock with Touch ID? [Y/n]"
4. If yes:
   a. Agent requests device key from Keychain (triggers Touch ID)
   b. On success: Agent decrypts identity.enc -> seed -> derives UK
   c. Agent is now unlocked
5. If no (or Touch ID fails or is unavailable):
   a. Fall back to mnemonic entry
```

This gives 1Password-style UX: type your mnemonic once on setup, then
Touch ID for daily use. If you lose your device, recover with the
mnemonic on a new device.

#### Platform support

| Platform | Biometric | Key storage |
|----------|-----------|-------------|
| macOS | Touch ID via Security.framework | Keychain (kSecAccessControlBiometryCurrentSet) |
| Linux | Not reliable (PAM/polkit varies) | secret-service (D-Bus), no biometric gating |
| Windows | Windows Hello (future) | Credential Manager |

macOS is the primary target. Linux falls back to passphrase/mnemonic.

### CLI Changes

```
# Current:
blu init /path           generates age keypair, prompts for passphrase

# After stage 3:
blu identity init        generates mnemonic, derives UK, sets up biometric
blu identity show        displays public key
blu identity recover     restores identity from mnemonic
blu init /path           initializes vault using current identity
```

Identity is global (per user, `~/.blu/`). Vault init is separate.

### Recovery Kit

`blu recovery-kit generate` displays the 24 words and optionally
saves to PDF. Standard BIP39 recovery: enter 24 words on a new
device, derive the same seed, get the same UK, access all vaults.

## File Structure

### Per-Vault (`.blu/`)

```
.blu/
  config.toml              backend config, settings
  vault.toml               vault identity (UUID, created date)
  keys/
    kek.toml               KEK metadata, version history
    kek_v0/
      wrapped.age          current KEK (age multi-recipient)
  invitations/             pending user invitations (stage 2)
  indexes/
    index.dat              plain index (v2 format with DEK header)
    blob_index.dat         blob index (v2 format with DEK header)
    tag_index.dat          tag index (v2 format with DEK header)
  data/
    a/ab/abc/abcd...       blob files (v2 format with DEK header)
```

### Per-User (`~/.blu/`)

```
~/.blu/
  config.toml              agent config (timeout profile, preferences)
  identity.toml            public key (safe to share)
  identity.enc             seed encrypted with device key (biometric)
  agent.sock               agent Unix socket
  agent.pid                agent PID file
```

## Implementation Order

| Step | What | Depends on | Files |
|------|------|------------|-------|
| 1a | Agent daemon (start, stop, socket, lifecycle) | nothing | new: `src/agent/mod.rs`, `src/agent/daemon.rs` |
| 1b | Agent protocol (JSON-RPC 2.0, encrypt/decrypt) | 1a | new: `src/agent/protocol.rs`, `src/agent/rpc.rs` |
| 1c | CLI integration (AgentBlackBox, auto-start) | 1b | modify: `src/cli/helpers.rs`, `src/age.rs` |
| 1d | unlock/lock/agent CLI commands | 1c | new: `src/cli/unlock.rs`, `src/cli/lock.rs`, `src/cli/agent.rs` |
| 1e | Timeout profiles, ~/.blu/config.toml | 1d | new: `src/agent/config.rs` |
| 2a | KEK generation, storage, wrapping (age) | 1e | new: `src/keys/kek.rs` |
| 2b | DEK generation, wrapping (ChaCha20-Poly1305) | 2a | new: `src/keys/dek.rs` |
| 2c | Blob/index v2 format (header with wrapped DEK) | 2b | modify: `src/blob.rs`, `src/io.rs` |
| 2d | Agent wrap_dek/unwrap_dek RPCs | 2c | modify: `src/agent/protocol.rs` |
| 2e | Migration command (v1 to v2) | 2d | new: `src/cli/migrate.rs` |
| 3a | BIP39 mnemonic generation + seed derivation | 2e | new: `src/keys/mnemonic.rs` |
| 3b | identity init/recover/show commands | 3a | new: `src/cli/identity.rs` |
| 3c | Biometric unlock (macOS Keychain + Touch ID) | 3b | new: `src/agent/biometric.rs` |
| 3d | Recovery kit generation | 3b | new: `src/cli/recovery.rs` |

## New Dependencies

| Crate | Purpose | Stage |
|-------|---------|-------|
| `daemonize` or `fork` | Agent process daemonization | 1 |
| `zeroize` | Secure memory zeroing for all secret types | 1 |
| `memsec` or `region` | mlock() support for key buffers | 1 |
| `chacha20poly1305` | DEK wrapping and data encryption | 2 |
| `bip39` | Mnemonic generation and validation | 3 |
| `hkdf` | Key derivation from seed (sha2 already present) | 3 |
| `security-framework` | macOS Keychain and Touch ID integration | 3 |

## Cryptographic Specifications

| Purpose | Algorithm | Parameters |
|---------|-----------|------------|
| Mnemonic entropy | CSPRNG | 256 bits (24 words) |
| Mnemonic to seed | PBKDF2-HMAC-SHA512 | 2048 rounds, salt = "mnemonic" + passphrase |
| Seed to UK | HKDF-SHA256 | salt = "blu-x25519-v1", info = "" |
| User Key | X25519 | 32-byte private, 32-byte public |
| KEK | CSPRNG | 256 bits |
| KEK wrapping | age | X25519-based, multi-recipient |
| DEK | CSPRNG | 256 bits |
| DEK wrapping | ChaCha20-Poly1305 | 12-byte nonce, 16-byte tag |
| Data encryption | ChaCha20-Poly1305 | 12-byte nonce, 16-byte tag |

## Security Considerations

### Threat Model

Protected against:
- Cloud provider reading data (all data encrypted client-side)
- Stolen device without agent running (secrets not in memory)
- Compromised single DEK (only one blob exposed)
- Compromised old KEK after rotation (new data uses new KEK)
- Removed user accessing new data (KEK rotated on removal)

Not protected against:
- Compromised device while agent is unlocked (attacker has UK)
- User reveals mnemonic under duress
- Compromised mnemonic (full access to all user's vaults)
- Quantum computers (X25519 is not post-quantum; symmetric layer is)

### Implementation Requirements

1. Use `zeroize` crate for all secret types
2. Use `mlock()` for agent's secret storage
3. Use OS CSPRNG (`getrandom`) for all key generation
4. Constant-time comparison for all secret comparisons
5. Never log keys, mnemonics, passphrases, or DEKs

## Migration Path

### v0.5 (current) to v1.0 (after all three stages)

Phase 1 (agent only): no data format changes. Existing vaults work
as-is. The agent just caches the decrypted identity in memory instead
of prompting every time.

Phase 2 (envelope encryption): `blu migrate` converts vaults from v1
format (age-encrypted blobs) to v2 format (KEK/DEK envelope). Old
format is still readable for backward compatibility.

Phase 3 (BIP39): `blu identity init` creates a new mnemonic-based
identity. Users can import their existing age key during init, or
start fresh and re-authorize on existing vaults.
