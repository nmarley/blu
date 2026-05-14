# AGENTS.md

Encrypted, deduplicated file archival CLI written in Rust (single crate, not a workspace).

## Commands

```sh
cargo build --release        # binary: target/release/blu
cargo test                   # all tests (inline #[cfg(test)] modules)
cargo test -- --ignored      # include slow scrypt-based tests
cargo clippy                 # lint (see allowed lints below)
cargo fmt -- --check         # format check (max_width = 100)
```

No CI workflows, pre-commit hooks, or codegen steps exist yet.

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

### File format (`src/v2format.rs`)

Files use magic `BLUB` (blob) or `BLUI` (index) followed by a wrapped DEK header and ChaCha20-Poly1305 encrypted payload.

### Agent daemon (`src/agent/`)

Started via hidden `blu __agent-daemon` subcommand. Communicates over `~/.blu/agent.sock` using length-prefixed JSON-RPC 2.0. The daemon holds decrypted keys in mlock'd memory and zeroizes on drop.

### Storage backends (`src/storage/`)

`Backend` trait with `Local` and `AmazonS3` implementations. Blobs are content-addressed by multihash, stored in a sharded directory tree (e.g., `d/dd4/dd4ce/dd4ce38e...`).

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
- `src/v2format.rs` -- envelope-encrypted file format (header parsing, read/write)
- `src/config.rs` -- vault config from `.blu/config.toml`
- `src/storage/` -- `Backend` trait, `Local`, `AmazonS3`
- `src/io.rs` -- `EncryptedSerializable` trait (serialize + compress + encrypt for indexes)

## Style and lint

- `max_width = 100` in `.rustfmt.toml`
- `#![warn(missing_docs)]` -- all public items need doc comments
- Allowed clippy lints: `uninlined_format_args`, `needless_lifetimes`, `items_after_test_module`
- Errors use `thiserror` via `BluError` enum in `src/error.rs`; prefer `BluError` variants over ad-hoc strings
- Secret types must derive `Zeroize`/`ZeroizeOnDrop`; never log keys, passphrases, or plaintext

## Testing

- All tests are inline `#[cfg(test)]` modules (no `tests/` integration test directory)
- `test/blu_secrets/` -- test age keypair (`blu.key`, `blu.pub`)
- `test/blocks/` -- fixture directories for chunking/block tests (t1-t7)
- `test/old/` -- legacy config format fixtures (t0-t6)
- Some tests are `#[ignore]` because scrypt key derivation is slow; run with `cargo test -- --ignored`
- `src/keys/pq_integration_test.rs` -- PQ pipeline tests, compiled only under `#[cfg(test)]`

## Design docs

- `ENVELOPE_ENCRYPTION_DESIGN.md` -- canonical KEK/DEK design reference
- `TODO.md` -- consolidated backlog

## Platform

macOS is the primary target. `security-framework` crate provides Touch ID / Keychain integration for biometric unlock. Linux falls back to passphrase/mnemonic (no biometric gating).
