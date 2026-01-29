# blu Production Readiness Plan

## Overview

Transform blu from a working prototype into a production-ready encrypted backup tool with proper key management, polished S3 support, and intuitive CLI workflows.

**Design Principles:**

- Git-inspired: `.blu/` lives alongside your data
- User owns their keys: no key escrow, no cloud key storage
- Encrypt everything: indexes and blobs both encrypted at rest
- S3 as primary backend: local is for development/testing

---

## Phase 0: Foundation (Pre-MVP Cleanup)

**Goal:** Clean slate for building on.

### 0.1 Remove Hardcoded Test Keys

- Remove `TEST_AGE_SECRET_KEY` from all CLI modules
- Commands fail gracefully if no key configured
- Test infrastructure uses fixtures properly

### 0.2 Fix Deprecation Warnings

- Replace `NaiveDateTime::from_timestamp` with `DateTime::from_timestamp`

### 0.3 Basic Error Types

- Create `src/error.rs` with `BluError` enum
- Migrate critical paths from `Box<dyn Error>` to proper types
- Better user-facing error messages

---

## Phase 1: Key Management (MVP)

**Goal:** Real encryption with user-controlled keys.

### 1.1 Key Generation

- Add `age::x25519::Identity::generate()` to create keypairs
- Store public key (recipient) in config
- Private key (identity) stored separately

### 1.2 Private Key Storage

- New file: `.blu/identity.age` (passphrase-encrypted private key)
- Use existing `passphrase_encrypt`/`passphrase_decrypt`
- Prompt for passphrase on operations that need decryption

### 1.3 Config Schema Update

```toml
[encryption]
recipient = "age1..." # public key
identity_file = "identity.age" # relative to .blu/

[backend]
type = "s3"
bucket = "my-bucket"
prefix = "vaults/photos"
region = "us-east-1" # optional, new field
```

### 1.4 Key Loading Flow

- `init`: Generate keypair, prompt for passphrase, save encrypted identity
- `init --key-file <path>`: Import existing age key
- `init --no-passphrase`: Skip passphrase (for automation)
- Runtime: Load identity, prompt for passphrase, cache in memory for session

### 1.5 BlackBox Integration

- Modify `BlackBox::new()` to accept loaded identity
- Add `BlackBox::from_identity(identity: age::x25519::Identity)`
- Remove all hardcoded key references

---

## Phase 2: S3 Backend Polish (MVP)

**Goal:** Robust, efficient S3 operations.

### 2.1 Shared Tokio Runtime

- Create runtime once in `AmazonS3::new()`
- Store `Arc<Runtime>` in struct
- Use `runtime.block_on()` for operations

### 2.2 Config Improvements

- Add optional `region` field to S3Config
- Support `AWS_REGION` env fallback
- Validate bucket access on backend init (optional flag)

### 2.3 Storage Trait Refinement

- Keep sync trait for now (simpler)
- Consider renaming `path: &Path` to `key: &str` for clarity
- Add `exists(&self, key: &str) -> bool` method
- Add `delete(&self, key: &str)` method for cleanup

### 2.4 End-to-End Testing

- Integration test with LocalStack or minio
- Test: init -> add -> encrypt -> restore cycle

---

## Phase 3: CLI Workflows (MVP)

**Goal:** Intuitive commands for common operations.

### 3.1 Porcelain Commands

**`blu init <dir>`** (enhance existing)

- Generate or import key
- Create config with backend selection
- Interactive prompts for S3 bucket/prefix

**`blu sync`** (new)

- Combines: update index + encrypt new chunks + push to backend
- Idempotent: safe to run repeatedly
- Shows summary: "Added 5 files, encrypted 12 chunks, uploaded 2 blobs"

**`blu status`** (enhance existing)

- Show files not yet indexed
- Show indexed files not yet encrypted
- Show local changes since last sync

**`blu restore <pattern>`** (enhance existing)

- Support path patterns: `blu restore "photos/*.jpg"`
- Support hash prefix: `blu restore 1340a2...`
- `blu restore --all` for full restore
- `--to <dir>` for restore destination

**`blu ls`** (alias/enhance list-files)

- Default: show paths
- `--long`: show hash, size, chunk count
- `--tags <tagspec>`: filter by tags

### 3.2 Plumbing Commands (keep existing)

- `write-index`, `read-index`, `encrypt-files`, etc.
- Add `--help` documentation
- Mark as "plumbing" in help text

### 3.3 Global Options

- `--verbose` / `-v`: detailed output
- `--quiet` / `-q`: minimal output
- `--no-passphrase`: don't prompt (fail if key is encrypted)
- `--config <path>`: alternate config location

---

## Phase 4: Dependency Updates (Polish)

**Goal:** Modern, maintained dependencies.

### 4.1 Critical Updates

- `age`: 0.7 -> 0.10+ (API changes, security fixes)
- `aws-sdk-s3`: 0.29 -> 1.x (major rewrite, better ergonomics)
- `aws-config`: 0.56 -> 1.x (matches SDK)

### 4.2 Moderate Updates

- `chrono`: update to avoid deprecation
- `clap`: 4.3 -> 4.5 (minor)
- `tokio`: 1.x (already current)

### 4.3 Minor Updates

- `simplelog`, `tempfile`, etc.

### 4.4 Dependency Audit

- Run `cargo audit`
- Address any security advisories

---

## Phase 5: Remote Index Support (Polish)

**Goal:** Access vault from any machine with the key.

### 5.1 Index Upload

- After `sync`, optionally push indexes to backend
- Indexes already encrypted, just need upload

### 5.2 Index Download

- `blu pull-index`: fetch latest indexes from backend
- Overwrites local (no merge for MVP)

### 5.3 Clone Command

- `blu clone <backend-uri> <local-dir>`
- Creates local `.blu/` with config
- Pulls indexes from backend
- User provides key separately

### 5.4 Conflict Awareness

- Track index version/timestamp
- Warn if remote is newer than local
- Refuse push if conflict detected (force flag to override)

---

## Phase 6: Future Enhancements

**Goal:** Long-term improvements (not blocking production).

### 6.1 Post-Quantum Encryption

- Monitor `age-plugin-ml-kem` development
- Design for hybrid mode (X25519 + ML-KEM)
- Migration path for existing vaults

### 6.2 Multi-Backend

- Support multiple backends in config
- Replicate blobs across backends
- Backend health checks

### 6.3 Performance

- Async storage trait
- Parallel chunk encryption
- Streaming for large files

### 6.4 UX Polish

- Progress bars for long operations
- Color output
- `blu doctor` for diagnosing issues

---

## Migration Notes

### Config Migration (v0.4 -> v0.5)

- Add `[encryption]` section
- Move backend to `[backend]` (already there)
- Tool: `blu migrate-config` or auto-detect on load

### Index Compatibility

- Index version already tracked (`CURRENT_INDEX_VERSION`)
- Bump version for any schema changes
- Support reading old versions, writing new

---

## Success Criteria

### MVP Complete When:

1. `blu init` generates real keys with passphrase protection
2. `blu sync` works end-to-end with S3
3. `blu restore` can recover files by path
4. No hardcoded keys or credentials in code
5. All tests pass, including S3 integration

### Polish Complete When:

1. All deps on latest stable versions
2. Remote index sync works
3. `cargo audit` clean
4. README updated with real usage examples
