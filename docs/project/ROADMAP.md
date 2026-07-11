# Roadmap

Pre-release roadmap for blu, an encrypted, deduplicated file archival CLI.

Versioning: crate and docs use **0.5.x** as the pre-release line. Milestones
below are internal 0.5.x goals, not a re-tag to v0.1.0-alpha.

## Current State

blu is a late-alpha, single-developer Rust project (~25k LOC under `src/`,
~350+ tests, zero clippy warnings). The core pipeline works end-to-end:
vault creation from a BIP39 mnemonic, content-addressed chunking with
deduplication, envelope encryption (PQ hybrid ML-KEM-768 + X25519), agent
daemon with macOS Touch ID unlock, multi-backend storage (local + S3) with
mirror/diff, concurrent restore, delete cascade with inline blob repacking,
search across filenames and tags, `.bluignore`, `blu doctor`, `blu serve`,
and GitHub Actions CI.

Differentiators over existing backup tools (restic, borg, duplicity):
post-quantum hybrid encryption by default, BIP39 mnemonic-based identity
recovery, three-tier envelope encryption with rotatable KEK, biometric
unlock via agent daemon, and a single static binary with no runtime
dependencies.

## 0.5.0 dogfood (landed)

- GitHub Actions CI (build + test + clippy + fmt on `macos-15` and
  `ubuntu-24.04`)
- README with features, install, quick-start
- Initial changelog (Keep a Changelog)
- `.bluignore` file support
- `blu doctor` diagnostics (structural + blob presence)

## 0.5.x next: multi-user and key lifecycle

- Multi-user access: invite, accept, remove (see
  `docs/design/ENVELOPE_ENCRYPTION_DESIGN.md`)
- KEK rotation CLI (`blu kek rotate`, `blu kek status`)
- Recovery kit generation (`blu recovery-kit generate`, optional PDF)

## 0.5.x later: operational maturity

- Additional storage backends (DigitalOcean Spaces, GCS, Azure Blob)
- Streaming index I/O (memory-mapped or streaming reads instead of
  loading full index into memory)
- Tombstone GC after multi-device LWW deletes (shipped: merge + tombstones)
- `--verbose` option for `list-files` (chunk count, size, encryption
  status)
- Color output
- Doctor: backend `list` + orphan blob detection / repair
- Broader per-command CLI test suite

## Beyond

Items that may land eventually but are not blocking any milestone.

- Configurable hashing algorithm with multihash support
- Snapshot/versioning model with retention policies
- File notes (larger text bodies than tags, searchable)
- Hardware key support (YubiKey/Ledger for UK storage)
- Vault sharing via URL
- Separate std/fs implementation from core API (accept bytes in lib,
  keep fs operations in tools layer)
- Web UI for browsing vaults

## Non-Goals

These are explicitly out of scope for the foreseeable future.

- FUSE mount support
- Backward compatibility with pre-0.5.0 formats
- Windows support (macOS primary, Linux secondary)
- GUI application
