# Stage 2: Envelope Encryption (KEK/DEK)

Detailed implementation plan for adding a proper key hierarchy to blu.
This document is self-contained: it captures all design decisions,
data structures, file layouts, affected code paths, and implementation
order so any session can pick it up cold.

## Prerequisites

Stage 1 (agent + session management) is complete:

- Agent daemon runs at `~/.blu/agent.sock`
- `BlackBox` is an enum: `InProcess` (age identities) or `Agent`
  (delegates to daemon)
- `blu unlock` / `blu lock` / `blu agent status|stop` work
- Timeout profiles (paranoid/balanced/relaxed/custom) auto-lock the
  agent after idle or max duration
- `helpers.rs` auto-starts the agent, prompts passphrase if locked,
  returns `BlackBox::Agent(client)` to all CLI commands transparently

## Goal

Separate the key hierarchy so data encryption keys are independent
from the user's identity:

```
User Key (UK)                    age X25519 identity (current)
   | age encryption (asymmetric)
   v
Key Encryption Key (KEK)         256-bit, one per vault, rotatable
   | ChaCha20-Poly1305 (symmetric)
   v
Data Encryption Key (DEK)        256-bit, one per blob/index file
   | ChaCha20-Poly1305 (symmetric)
   v
Encrypted Data
```

Benefits:
- Key rotation without re-encrypting data (re-wrap DEKs only)
- Multi-user access (wrap KEK to multiple age recipients)
- PQ readiness (only UK layer needs changing; symmetric is safe)

## New Dependency

```toml
chacha20poly1305 = "0.10"
```

Used for: KEK wraps DEK, DEK encrypts data. Both symmetric.

## Sub-stages

| Step | What | New/modified files |
|------|------|--------------------|
| 2a | KEK generation, storage, wrapping | new: `src/envelope/mod.rs`, `src/envelope/kek.rs` |
| 2b | DEK generation, wrapping, data encrypt/decrypt | new: `src/envelope/dek.rs` |
| 2c | v2 file format (header + DEK-encrypted payload) | new: `src/envelope/format.rs`; modify: `src/blob.rs`, `src/io.rs` |
| 2d | Agent wrap_dek/unwrap_dek RPCs + KEK caching | modify: `src/agent/protocol.rs`, `src/agent/daemon.rs`, `src/agent/state.rs`, `src/agent/client.rs` |


## 2a: KEK Generation, Storage, Wrapping

### On-disk layout

```
.blu/keys/
  kek.toml                metadata (current version, authorized users)
  kek_v0/
    wrapped.age           KEK encrypted to authorized age recipients
```

### kek.toml schema

```toml
current_version = 0
created = "2026-03-07T12:00:00Z"

[[versions]]
version = 0
created = "2026-03-07T12:00:00Z"
status = "active"                   # active | deprecated | archived
users = ["age1alice..."]
```

### Data structures (`src/envelope/kek.rs`)

```rust
/// On-disk KEK metadata.
#[derive(Serialize, Deserialize)]
pub struct KekManifest {
    pub current_version: u16,
    pub created: String,
    pub versions: Vec<KekVersion>,
}

#[derive(Serialize, Deserialize)]
pub struct KekVersion {
    pub version: u16,
    pub created: String,
    pub status: KekStatus,
    pub users: Vec<String>,  // age1... public keys
}

#[derive(Serialize, Deserialize, PartialEq)]
pub enum KekStatus {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "deprecated")]
    Deprecated,
    #[serde(rename = "archived")]
    Archived,
}
```

### Functions to implement

```rust
/// Generate a new random 256-bit KEK using OS CSPRNG.
pub fn generate_kek() -> [u8; 32]

/// Wrap (encrypt) a KEK for a set of age recipients.
/// Uses age multi-recipient encryption.
/// Returns the age-encrypted ciphertext bytes.
pub fn wrap_kek(
    kek: &[u8; 32],
    recipients: &[&str],  // age1... public key strings
) -> Result<Vec<u8>>

/// Unwrap (decrypt) a KEK using an age identity.
/// Returns the 32-byte plaintext KEK.
pub fn unwrap_kek(
    wrapped: &[u8],
    identity: &age::x25519::Identity,
) -> Result<[u8; 32]>

/// Initialize KEK storage for a vault.
/// Creates .blu/keys/, generates KEK v0, wraps for the given
/// recipient, writes kek.toml and kek_v0/wrapped.age.
pub fn init_kek(
    blu_dir: &Path,         // path to .blu/
    recipient: &str,        // age1... public key
) -> Result<()>

/// Load the KEK manifest from .blu/keys/kek.toml.
pub fn load_manifest(blu_dir: &Path) -> Result<KekManifest>

/// Read the wrapped KEK bytes for a given version.
pub fn read_wrapped_kek(
    blu_dir: &Path,
    version: u16,
) -> Result<Vec<u8>>

/// Check whether a vault has KEK storage initialized.
pub fn has_kek(blu_dir: &Path) -> bool
```

### Where KEK init happens

During `blu init`, after generating the age keypair. The init command
will call `init_kek()` to create the key hierarchy. Existing vaults
that predate stage 2 will not have `.blu/keys/`; the v1 fallback
handles them.

### Integration with `blu init`

Modify `src/cli/init.rs`:
- After generating/importing the age identity and writing config,
  call `envelope::kek::init_kek(&blu_dir, &recipient_str)`
- The KEK is wrapped to the vault owner's public key

### Tests (2a)

- `generate_kek` produces 32 random bytes, two calls differ
- `wrap_kek` / `unwrap_kek` round-trip
- `wrap_kek` with multiple recipients, each can unwrap
- `init_kek` creates the expected directory structure and files
- `load_manifest` reads back what `init_kek` wrote
- `has_kek` returns false before init, true after

## 2b: DEK Generation, Wrapping, Data Encrypt/Decrypt

### Data structures (`src/envelope/dek.rs`)

```rust
/// A plaintext DEK (zeroized on drop).
pub struct Dek {
    key: [u8; 32],  // uses zeroize::Zeroize on drop
}

/// A wrapped (encrypted) DEK: nonce + ciphertext + tag.
/// This is what gets stored in the file header.
pub struct WrappedDek {
    pub kek_version: u16,
    pub bytes: Vec<u8>,  // 12-byte nonce || 32-byte ciphertext || 16-byte tag = 60 bytes
}
```

### Functions to implement

```rust
/// Generate a new random 256-bit DEK.
pub fn generate_dek() -> Dek

/// Wrap a DEK with a KEK using ChaCha20-Poly1305.
/// Returns nonce || ciphertext || tag (60 bytes).
pub fn wrap_dek(
    dek: &Dek,
    kek: &[u8; 32],
) -> Result<Vec<u8>>

/// Unwrap a DEK using a KEK.
pub fn unwrap_dek(
    wrapped: &[u8],     // 60 bytes: nonce || ciphertext || tag
    kek: &[u8; 32],
) -> Result<Dek>

/// Encrypt data with a DEK using ChaCha20-Poly1305.
/// Generates a random nonce, prepends it to the ciphertext.
/// Output: 12-byte nonce || ciphertext || 16-byte tag
pub fn encrypt_data(
    data: &[u8],
    dek: &Dek,
) -> Result<Vec<u8>>

/// Decrypt data with a DEK.
/// Input: 12-byte nonce || ciphertext || 16-byte tag
pub fn decrypt_data(
    encrypted: &[u8],
    dek: &Dek,
) -> Result<Vec<u8>>
```

### Tests (2b)

- `generate_dek` produces distinct keys
- `wrap_dek` / `unwrap_dek` round-trip with known KEK
- `unwrap_dek` with wrong KEK fails
- `encrypt_data` / `decrypt_data` round-trip
- `decrypt_data` with wrong DEK fails
- `decrypt_data` with tampered ciphertext fails (AEAD tag check)

## 2c: v2 File Format

### Header layout

```
Offset   Size     Field
0        4        Magic: "BLUB" (blobs) or "BLUI" (indexes)
4        2        Format version: 2 (LE u16)
6        2        KEK version (LE u16)
8        4        Wrapped DEK length N (LE u32)
12       N        Wrapped DEK bytes (nonce || ciphertext || tag)
12+N     ...      DEK-encrypted payload (nonce || ciphertext || tag)
```

Total header overhead: 12 + 60 = 72 bytes for a standard wrapped DEK.

### Backward compatibility

v1 files have no magic header. age-encrypted data starts with
`"age-encryption.org"` (ASCII). Detection:

```rust
fn detect_format(data: &[u8]) -> FileFormat {
    if data.len() >= 4 {
        match &data[0..4] {
            b"BLUB" | b"BLUI" => FileFormat::V2,
            _ => FileFormat::V1,
        }
    } else {
        FileFormat::V1
    }
}
```

### Data structures (`src/envelope/format.rs`)

```rust
pub const MAGIC_BLOB: &[u8; 4] = b"BLUB";
pub const MAGIC_INDEX: &[u8; 4] = b"BLUI";
pub const FORMAT_VERSION: u16 = 2;

pub enum FileFormat {
    V1,
    V2,
}

pub struct V2Header {
    pub magic: [u8; 4],
    pub version: u16,
    pub kek_version: u16,
    pub wrapped_dek: Vec<u8>,
}
```

### Functions to implement

```rust
/// Write a v2 header to a writer.
pub fn write_header<W: Write>(
    writer: &mut W,
    magic: &[u8; 4],
    kek_version: u16,
    wrapped_dek: &[u8],
) -> Result<()>

/// Read and parse a v2 header from a reader.
/// Returns None if the data is v1 format.
pub fn read_header<R: Read>(
    reader: &mut R,
    peek: &[u8],         // first 4 bytes already read for detection
) -> Result<Option<V2Header>>

/// Detect file format from the first bytes.
pub fn detect_format(data: &[u8]) -> FileFormat

/// Encrypt data in v2 format: header + DEK-encrypted payload.
/// This is the high-level "write" operation.
pub fn v2_encrypt(
    data: &[u8],         // plaintext (already compressed)
    magic: &[u8; 4],
    kek: &[u8; 32],
    kek_version: u16,
) -> Result<Vec<u8>>

/// Decrypt data: detects v1/v2, dispatches accordingly.
/// For v2: parse header, unwrap DEK with provided KEK, decrypt.
/// For v1: delegate to the BlackBox (age-based).
/// The `kek_lookup` closure resolves a kek_version to plaintext KEK.
pub fn decrypt_auto(
    data: &[u8],
    bbox: &BlackBox,
    kek_lookup: impl Fn(u16) -> Result<[u8; 32]>,
) -> Result<Vec<u8>>
```

### Changes to `src/blob.rs`

#### BlobBuffer (write path)

Currently `roll_new_blob()` does:
```rust
let compressed = compress(&self.data)?;
let encrypted = self.bbox.encrypt(&compressed)?;
```

After 2c, when the vault has KEK storage:
```rust
let compressed = compress(&self.data)?;
let encrypted = if self.has_envelope {
    envelope::format::v2_encrypt(&compressed, MAGIC_BLOB, &self.kek, self.kek_version)?
} else {
    self.bbox.encrypt(&compressed)?  // v1 fallback
};
```

`BlobBuffer` gains two new fields:
- `kek: Option<[u8; 32]>` (plaintext KEK, cached from agent)
- `kek_version: u16`

These are set during construction when the vault has KEK storage.

#### EncBlobReader (read path)

Currently `get_bytes()` does:
```rust
let data = self.bbox.decrypt(data)?;
let data = decompress(&data)?;
```

After 2c:
```rust
let data = envelope::format::decrypt_auto(
    data,
    self.bbox,
    |v| self.resolve_kek(v),
)?;
let data = decompress(&data)?;
```

Wait: for v2, the data stored is `header + DEK_encrypt(compressed)`.
So we need to detect format, strip/parse header, unwrap DEK, then
decrypt. The decompress step is inside the encrypted payload. Let me
correct the flow:

v1 write: `compress -> age_encrypt -> write`
v1 read: `read -> age_decrypt -> decompress`

v2 write: `compress -> dek_encrypt -> prepend_header -> write`
v2 read: `read -> detect_format -> parse_header -> unwrap_dek -> dek_decrypt -> decompress`

The compression step stays the same. Only the encryption layer changes.

For `decrypt_auto`, the returned bytes are already decompressed? No.
Let me re-examine. The v1 path: `bbox.decrypt()` returns compressed
bytes, then caller decompresses. For v2, `dek_decrypt()` should also
return compressed bytes (the payload before compression is applied by
the caller). So the encrypt/decrypt swap is purely at the crypto layer.

Corrected `decrypt_auto`: returns the decrypted payload (still
compressed). The caller handles decompression, same as v1.

### Changes to `src/io.rs`

The `gen_std_bbserde!` macro generates `write` and `read` methods
for index types (PlainIndex, BlobIndex, TagIndex). These currently
call `bbox.encrypt()` and `bbox.decrypt()`.

After 2c, these need to handle v2 format too. The macro needs to
accept optional KEK context. Options:

**Option A**: Add an `EncryptionContext` that wraps either a BlackBox
(v1) or a BlackBox + KEK (v2). Pass this instead of `&BlackBox`.

**Option B**: Keep `&BlackBox` but extend it with an optional KEK
field. When the KEK is present, `encrypt()` and `decrypt()` use v2
format automatically.

**Option C**: Change the macro to accept a new trait that abstracts
over v1 and v2 encryption. This is the cleanest but biggest change.

**Recommended: Option B.** Add a `with_envelope()` method to BlackBox
that attaches KEK context. When `BlackBox::encrypt()` is called and
a KEK is attached, it uses v2 format (generate DEK, wrap with KEK,
ChaCha20 the data). When `BlackBox::decrypt()` is called, it detects
v1/v2 from the magic bytes and dispatches accordingly. For v2 decrypt,
it uses the attached KEK to unwrap the DEK.

This means the `gen_std_bbserde!` macro, `BlobBuffer`, `EncBlobReader`,
and all config index macros continue calling `bbox.encrypt()` and
`bbox.decrypt()` without changes. The v2 logic is hidden inside
BlackBox.

Concretely, add to `BlackBox`:

```rust
pub struct EnvelopeContext {
    pub kek: [u8; 32],
    pub kek_version: u16,
    pub magic: [u8; 4],
}

impl BlackBox {
    /// Attach envelope encryption context (KEK).
    /// When set, encrypt() produces v2 format and decrypt() handles
    /// both v1 and v2 automatically.
    pub fn set_envelope(&mut self, ctx: EnvelopeContext) { ... }

    /// Clear the envelope context.
    pub fn clear_envelope(&mut self) { ... }
}
```

For the `Agent` variant: the agent already has the KEK cached. The
wrap_dek/unwrap_dek RPCs handle KEK operations. But we still want the
actual data encryption (ChaCha20 with the DEK) to happen in-process
for performance. So the flow for `Agent` + v2:

encrypt():
1. Call agent's `wrap_dek` RPC (returns plaintext DEK + wrapped DEK)
2. Encrypt data in-process with DEK using ChaCha20-Poly1305
3. Prepend v2 header (magic, version, kek_version, wrapped DEK)
4. Return header + ciphertext

decrypt():
1. Detect v1/v2 from magic bytes
2. If v1: delegate to agent's `decrypt` RPC (existing path)
3. If v2: parse header, call agent's `unwrap_dek` RPC, decrypt
   data in-process with DEK

This is the key efficiency win: only the 32-byte DEK traverses the
socket, not the entire data payload.

For the `InProcess` variant + v2: same logic but KEK operations
happen in-process using the attached EnvelopeContext.

### Where the magic byte differs (blob vs index)

`BlobBuffer` always writes blob files: magic = `BLUB`.
Index files (PlainIndex, BlobIndex, TagIndex) use magic = `BLUI`.

The `gen_std_bbserde!` macro writes indexes. Currently the macro gets
`&BlackBox`. For v2, the BlackBox needs to know which magic to use.

Solution: add `set_magic(&mut self, magic: [u8; 4])` or pass magic
via the EnvelopeContext. Since the EnvelopeContext already has a
`magic` field, callers set it appropriately:

- `BlobBuffer` sets `magic = BLUB` on its BlackBox
- Index write methods set `magic = BLUI` on their BlackBox

Alternatively, the `write` and `read` methods in the macro could
pass magic explicitly. But that changes the trait signature. Keeping
it in the BlackBox is less invasive.

## 2d: Agent wrap_dek / unwrap_dek RPCs

### New RPC methods

Add to `src/agent/protocol.rs`:

```rust
WrapDek,
UnwrapDek,
```

### Agent KEK caching (`src/agent/state.rs`)

The agent needs to cache per-vault KEKs. On first `wrap_dek` or
`unwrap_dek` for a vault, the agent:

1. Reads `.blu/keys/kek.toml` from the vault path
2. Reads the wrapped KEK for the requested version
3. Unwraps it using the cached age identity
4. Caches the plaintext KEK keyed by (vault_path, kek_version)

```rust
/// Per-vault KEK cache in AgentState.
struct VaultKekCache {
    /// vault_path -> (kek_version -> plaintext KEK)
    cache: HashMap<PathBuf, HashMap<u16, [u8; 32]>>,
}
```

All cached KEKs are zeroized on lock.

### wrap_dek RPC

Request:
```json
{
    "method": "wrap_dek",
    "params": { "vault_path": "/path/to/.blu" }
}
```

Handler:
1. Resolve KEK for the vault (cache or decrypt from disk)
2. Generate random DEK
3. Wrap DEK with KEK using ChaCha20-Poly1305
4. Return plaintext DEK (base64), wrapped DEK (base64), kek_version

Response:
```json
{
    "result": {
        "dek": "<base64-32-bytes>",
        "wrapped_dek": "<base64-60-bytes>",
        "kek_version": 0
    }
}
```

### unwrap_dek RPC

Request:
```json
{
    "method": "unwrap_dek",
    "params": {
        "vault_path": "/path/to/.blu",
        "wrapped_dek": "<base64-60-bytes>",
        "kek_version": 0
    }
}
```

Handler:
1. Resolve KEK for the vault and version (cache or decrypt)
2. Unwrap DEK from the wrapped bytes
3. Return plaintext DEK (base64)

Response:
```json
{
    "result": {
        "dek": "<base64-32-bytes>"
    }
}
```

### Client convenience methods (`src/agent/client.rs`)

```rust
pub fn wrap_dek(&self, vault_path: &str) -> Result<WrapDekResult>
pub fn unwrap_dek(&self, vault_path: &str, wrapped: &[u8], version: u16) -> Result<Vec<u8>>
```

Where `WrapDekResult` contains `dek: Vec<u8>`, `wrapped_dek: Vec<u8>`,
`kek_version: u16`.

### Tests (2d)

- wrap_dek returns valid DEK + wrapped DEK
- unwrap_dek round-trips with wrap_dek
- wrap_dek fails when agent is locked
- unwrap_dek with wrong kek_version fails gracefully
- KEK is cached (second call to same vault does not read disk)

## Module Layout

New module: `src/envelope/`

```
src/envelope/
  mod.rs        re-exports
  kek.rs        KEK generation, wrapping, storage
  dek.rs        DEK generation, wrapping, data encrypt/decrypt
  format.rs     v2 file header read/write, format detection
```

Register in `src/lib.rs`:

```rust
/// envelope encryption (KEK/DEK key hierarchy)
pub mod envelope;
```

## Changes to Existing Files (Summary)

| File | Change |
|------|--------|
| `Cargo.toml` | Add `chacha20poly1305 = "0.10"` |
| `src/lib.rs` | Add `pub mod envelope` |
| `src/age.rs` | Add `EnvelopeContext`, `set_envelope()`, v2 logic in `encrypt()`/`decrypt()` |
| `src/blob.rs` | `BlobBuffer` and `EncBlobReader` gain optional envelope context |
| `src/io.rs` | No changes needed (BlackBox handles v1/v2 internally) |
| `src/config.rs` | No changes needed (index macros use `&BlackBox`) |
| `src/cli/init.rs` | Call `init_kek()` after keypair generation |
| `src/cli/helpers.rs` | After loading BlackBox, attach envelope context if vault has KEK |
| `src/agent/protocol.rs` | Add `WrapDek`, `UnwrapDek` methods |
| `src/agent/daemon.rs` | Add `handle_wrap_dek`, `handle_unwrap_dek` |
| `src/agent/state.rs` | Add `VaultKekCache`, resolve/cache KEK per vault |
| `src/agent/client.rs` | Add `wrap_dek()`, `unwrap_dek()` convenience methods |

## Implementation Order (Detailed)

### 2a (KEK)
1. Add `chacha20poly1305` to Cargo.toml
2. Create `src/envelope/mod.rs`
3. Create `src/envelope/kek.rs` with generate/wrap/unwrap/init/load
4. Register `pub mod envelope` in `src/lib.rs`
5. Write tests for all KEK functions
6. Build + test
7. Stop for review

### 2b (DEK)
1. Create `src/envelope/dek.rs` with generate/wrap/unwrap/encrypt/decrypt
2. Write tests for all DEK functions
3. Build + test
4. Stop for review

### 2c (v2 format + BlackBox integration)
1. Create `src/envelope/format.rs` with header read/write, detection
2. Add `EnvelopeContext` to `BlackBox` in `src/age.rs`
3. Update `BlackBox::encrypt()` to produce v2 when envelope is set
4. Update `BlackBox::decrypt()` to detect v1/v2 and dispatch
5. Write tests for format + integrated encrypt/decrypt
6. Build + test (all existing tests must still pass with v1 path)
7. Stop for review

### 2d (Agent RPCs)
1. Add `WrapDek`/`UnwrapDek` to `src/agent/protocol.rs`
2. Add `VaultKekCache` to `src/agent/state.rs`
3. Add handlers to `src/agent/daemon.rs`
4. Add client convenience methods to `src/agent/client.rs`
5. Update `BlackBox::Agent` encrypt/decrypt to use wrap/unwrap RPCs
6. Write tests
7. Build + test
8. Stop for review

## Open Questions

1. **Should `blu init` on a new vault always create KEK storage?**
   Proposed: yes. New vaults always get v2.

2. **Should the agent cache the plaintext KEK, or should the CLI
   hold it?** Proposed: agent caches it. The CLI never sees the KEK
   directly; it only gets DEKs back from wrap/unwrap RPCs. However,
   for `InProcess` BlackBox (no agent), the KEK is loaded directly
   in the CLI process.

3. **What is the maximum wrapped DEK size?** ChaCha20-Poly1305 with
   a 32-byte plaintext produces: 12 (nonce) + 32 (ciphertext) + 16
   (tag) = 60 bytes. This is fixed and can be validated.
