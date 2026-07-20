# PLAN: Blake3 content hashing (greenfield)

Replace SHA-512 content addressing with Blake3-256 multihash.
No migration, no dual-algorithm support, no config switch, no
backward compatibility. Existing vaults are not supported after
this change; re-init / re-backup is the path.

KDF and key-derivation hashes (HKDF-SHA256, PBKDF2-SHA512, SHA3,
SHAKE256, index ciphertext SHA-256) are out of scope and stay as-is.

## Decision

- Content IDs (chunk, whole-file, encrypted blob): Blake3-256
- Wire/storage envelope: multihash, multicodec `blake3-256` (`0x1e`)
- Digest length: 32 bytes (multihash bytes typically 34 total)
- Streaming: `blake3::Hasher` via `StreamingHash`
- Callers must go through `src/hash.rs` only (no direct hasher use
  for content IDs outside that module)

## Stages

Stage 1: Depend on `blake3` and rewrite `src/hash.rs`
  1a: Add `blake3` to `Cargo.toml`
  1b: Replace `SHA2_512` / `sha512` with `BLAKE3_256` (`0x1e`) and
      Blake3 digest helpers
  1c: Point `multihash()` at Blake3-256 + `Multihash::wrap`
  1d: Rewrite `StreamingHash` on `blake3::Hasher` (`update`,
      `finalize` -> multihash `Hash`, `finalize_raw` -> 32-byte digest)
  1e: Drop content-plane SHA-512 helpers that are no longer used
  1f: Keep `Hash` type and Debug/Display behavior (parse multihash
      header; do not assume fixed header width)

Stage 2: Remove bypass call sites
  2a: `src/block/index.rs` whole-file hash: use `StreamingHash`
      (or `hash::multihash` on buffered data), delete direct
      `Sha512` / `SHA2_512` usage
  2b: `src/cli/restore.rs` whole-file verify path: same
  2c: Grep for `Sha512`, `SHA2_512`, and raw content digests;
      only `sha2::Sha256` (and other KDF uses) may remain

Stage 3: Fix tests and hardcoded digests
  3a: Update unit/smoke fixtures that embed SHA-512 multihash hex
  3b: Update `storage::path_for` tests if any assume SHA-512-only
      production output (path sharding stays algorithm-agnostic)
  3c: `cargo test` green (including serve/redb paths that key on
      multihash bytes)

Stage 4: Docs and backlog cleanup
  4a: `docs/design/BLU_SERVE_DESIGN.md`: write path says Blake3-256
      multihash
  4b: `docs/project/TODO.md` / `ROADMAP.md`: drop or rewrite
      "configurable hashing with backward compat" items that this
      greenfield cut supersedes (no dual-hash table work)
  4c: `AGENTS.md` / `README.md` only if they claim SHA-512 content
      addressing
  4d: `CHANGELOG.md` unreleased note: content hash is Blake3-256;
      vault format break; re-backup required

Stage 5: Verify
  5a: `cargo test`
  5b: `cargo clippy`
  5c: `cargo fmt -- --check`
  5d: Quick manual smoke: init, backup, restore, doctor on a fresh
      vault

## Non-goals

- Migrating or reading pre-Blake3 vaults
- Configurable or pluggable hash algorithms
- Global int-ID hash table
- Changing chunk size, CDC, compression, or AEAD
- Touching KDF / identity / envelope-encryption hash choices
- Parallel/tree Blake3 whole-file hashing (serial streaming is enough)

## Suggested commit split

One atomic commit per stage (1 through 4). Stage 5 is verification
only, no commit unless docs/tests needed a fixup.
