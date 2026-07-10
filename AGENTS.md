# AGENTS.md

Encrypted, deduplicated file archival CLI written in Rust (single crate, not a workspace).

## Commands

```sh
cargo build --release        # binary: target/release/blu
cargo test                   # all tests (inline #[cfg(test)] modules)
cargo test -- --ignored      # include slow scrypt-based tests
cargo clippy                 # lint (see allowed lints below)
cargo fmt -- --check         # format check (max_width = 100)
bash scripts/check-version.sh  # crate version vs latest v* tag
bash scripts/install-local.sh  # cargo install --path . --force (+ macOS codesign)
```

CI: `.github/workflows/ci.yml` on push/PR (`macos-15`, `ubuntu-24.04`).
No pre-commit hooks or codegen steps.

Optional local pre-push hook (fast shell only, no cargo build). Runs
`scripts/check-version.sh` and rejects `vX.Y.Z` tag pushes when the
tagged commit's `Cargo.toml` version is not `X.Y.Z`:

```sh
git config core.hooksPath .githooks
```

Version rules: `Cargo.toml` package version must be >= latest `v*` tag;
a pushed `vX.Y.Z` tag must match `Cargo.toml` at that commit.

## Greenfield rules

This is a solo-developer project with no external users. Breaking changes are welcome and preferred when they produce a cleaner design. Do not preserve backward compatibility, migration paths, fallback code paths, or deprecation shims unless explicitly asked. When in doubt, delete the old thing.

## Architecture

### DekProvider abstraction (`src/dek_provider.rs`)

`DekProvider` is the key management seam for envelope encryption. It is a concrete enum with two variants:

- **Local**: holds an unwrapped KEK in-process. Used only during `blu init` (vault creation).
- **Agent**: delegates DEK wrapping/unwrapping to the agent daemon over a Unix socket. Key material never leaves the daemon.

All bulk data encryption is local (ChaCha20-Poly1305 with the DEK). `DekProvider` controls only who wraps/unwraps the DEK. Free functions `encrypt_envelope()` and `decrypt_envelope()` handle the full envelope format. The seam is in `src/cli/helpers.rs` (`load_config_and_keys()`).

### Key hierarchy (envelope encryption)

```
User Key (PQ hybrid: ML-KEM-768 + X25519, from BIP39 mnemonic)
  -> wraps KEK (one per vault, age asymmetric, rotatable)
    -> wraps DEK (one per blob/index, ChaCha20-Poly1305)
      -> encrypts data (ChaCha20-Poly1305)
```

Only the top layer (UK wraps KEK) uses asymmetric crypto. Everything below is symmetric and already quantum-resistant.

### File format

- Indexes (`BLUI`): v2 envelope, gzip + CBOR (ciborium) + ChaCha20-Poly1305
- New blobs (`BLUB`): v3 segmented AEAD (`src/v3format.rs`); v2 still readable
- Header parsing helpers also in `src/v2format.rs`

### Agent daemon (`src/agent/`)

Started via hidden `blu __agent-daemon` subcommand. Communicates over
`$XDG_RUNTIME_DIR/blu/agent.sock` (falls back to `$XDG_STATE_HOME/blu/`
when runtime dir is unset) using length-prefixed JSON-RPC 2.0. PID file
is `$XDG_STATE_HOME/blu/agent.pid`. The daemon holds decrypted keys in
mlock'd memory and zeroizes on drop.

User-global paths (identity, agent, agent config) are resolved by
`src/user_paths.rs` via XDG Base Directory on all platforms (including
macOS). Defaults: `~/.config/blu`, `~/.local/share/blu`,
`~/.local/state/blu`. Vault-local state remains under project `.blu/`.

### Storage backends (`src/storage/`)

`BackendKind` enum (concrete dispatch, not a trait, because native async fn in traits is not object-safe) with `Local` and `AmazonS3` variants. Blobs are content-addressed by multihash, stored in a sharded directory tree (e.g., `d/dd4/dd4ce/dd4ce38e...`).

## Code layout

- `src/bin/blu.rs` -- CLI entrypoint, clap dispatch, vault discovery (walks parents for `.blu/`)
- `src/cli/` -- one file per subcommand; `clapargs.rs` defines all clap structs
- `src/cli/helpers.rs` -- constructs `DekProvider` (agent or local); key seam
- `src/dek_provider.rs` -- `DekProvider` enum, `encrypt_envelope`/`decrypt_envelope`
- `src/age.rs` -- passphrase-based encryption for identity files
- `src/keys/` -- KEK, DEK, BIP39 mnemonic, PQ hybrid KEM (mlkem768x25519), HPKE
- `src/agent/` -- daemon lifecycle, Unix socket protocol, biometric (macOS Touch ID), memlock
- `src/block/` -- chunking, block index, file references (deduplication layer)
- `src/blob.rs` -- blob packing (multiple chunks into one encrypted blob)
- `src/ignore.rs` -- `.bluignore` walking (`ignore` crate)
- `src/v2format.rs` -- envelope-encrypted file format (header parsing, read/write)
- `src/v3format.rs` -- v3 segmented AEAD blob format
- `src/serve/` -- `blu serve` local daemon (HTTP server, redb index store, index sync)
- `src/config.rs` -- vault config from `.blu/config.toml`
- `src/user_paths.rs` -- XDG paths for user-global identity/agent state
- `src/storage/` -- `BackendKind` enum, `Local`, `AmazonS3`
- `src/io.rs` -- `EncryptedSerializable` trait (serialize + compress + encrypt for indexes)

## Style and lint

- `max_width = 100` in `.rustfmt.toml`
- `#![warn(missing_docs)]` -- all public items need doc comments
- Allowed clippy lints: `uninlined_format_args`, `needless_lifetimes`, `items_after_test_module`
- Errors use `thiserror` via `BluError` enum in `src/error.rs`; prefer `BluError` variants over ad-hoc strings
- Secret types must derive `Zeroize`/`ZeroizeOnDrop`; never log keys, passphrases, or plaintext

## Testing

- All tests are inline `#[cfg(test)]` modules (no `tests/` integration test directory)
- `src/cli/smoke.rs` -- end-to-end vault pipeline smokes (init/sync/restore/delete/doctor)
- `test/blu_secrets/` -- classic age X25519 fixtures (`AGE-SECRET-KEY-1...` / `age1...`); not PQ; legacy/fixture use only
- `test/blocks/` -- fixture directories for chunking/block tests (t1-t7)
- `test/old/` -- legacy config format fixtures (t0-t6)
- Some tests are `#[ignore]` because scrypt key derivation is slow; run with `cargo test -- --ignored`
- `src/keys/pq_integration_test.rs` -- PQ pipeline tests, compiled only under `#[cfg(test)]`

## Design docs

- `docs/design/ENVELOPE_ENCRYPTION_DESIGN.md` -- canonical KEK/DEK design reference
- `docs/design/BLU_SERVE_DESIGN.md` -- `blu serve` local daemon design (S3-compatible API, redb index, segmented AEAD)
- `docs/project/START-HERE.md` -- living project status
- `docs/project/TODO.md` -- consolidated backlog
- `CHANGELOG.md` -- release notes

## Platform

macOS is the primary target. `security-framework` is a **macOS-only** Cargo dependency (Touch ID / Keychain). Linux falls back to passphrase/mnemonic (no biometric gating).
