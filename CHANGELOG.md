# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-07-09

Pre-release dogfood surface. Breaking changes remain expected.

### Added

- Envelope encryption with PQ hybrid user keys (ML-KEM-768 + X25519)
  and ChaCha20-Poly1305 bulk data
- BIP39 24-word global identity (`identity init` / `show` / `recover`)
- Agent daemon with unlock/lock, macOS Touch ID gating when available
- Content-addressed chunking, local and Amazon S3 backends
- Named multi-backend config with `backend mirror` and `backend diff`
- v3 segmented AEAD blob format with prefix-fetch reads; v2 readable;
  `defrag-blobs --upgrade-format` for migration
- `blu serve` local S3-compatible HTTP API over the encrypted vault
- Full delete cascade and blob defrag / repack
- `.bluignore` (gitignore-style) for add, sync, and status walks
- `blu doctor` vault health diagnostics
- End-to-end vault pipeline smoke tests
- GitHub Actions CI on `macos-15` and `ubuntu-24.04`

### Changed

- Index serialization uses CBOR via ciborium (not bincode)
- Scrypt work factor pinned to a minimum of 18 for identity files
- Plumbing commands (`write-index`, `encrypt-files`, `read-index`)
  hidden from help; obsolete `debug-index` removed
- `security-framework` is a macOS-only dependency

### Security

- v3 segment AAD binds header fields (segment size, count, plaintext
  length) in addition to the segment index
