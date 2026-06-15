# Roadmap

Pre-release roadmap for blu, an encrypted, deduplicated file archival CLI.

## Current State

blu is a late-alpha, single-developer Rust project (~15k lines, 200+ tests,
zero clippy warnings). The core pipeline works end-to-end: vault creation
from a BIP39 mnemonic, content-addressed chunking with deduplication,
envelope encryption (PQ hybrid ML-KEM-768 + X25519), agent daemon with
macOS Touch ID unlock, multi-backend storage (local + S3) with mirror/diff,
concurrent restore, delete cascade with inline blob repacking, and search
across filenames and tags.

Differentiators over existing backup tools (restic, borg, duplicity):
post-quantum hybrid encryption by default, BIP39 mnemonic-based identity
recovery, three-tier envelope encryption with rotatable KEK, biometric
unlock via agent daemon, and a single static binary with no runtime
dependencies.

## v0.1.0-alpha

Minimum viable release. Everything needed to tag a version you could
point someone at.

- GitHub Actions CI (cargo build + cargo test + cargo clippy on push)
- README with features, install instructions, quick-start guide
- Initial changelog entry (keepachangelog format)
- `.bluignore` file support (similar to `.gitignore`)
- `blu doctor` diagnostics (index integrity, blob health, backend
  reachability)

## v0.2.0-alpha

Multi-user and key lifecycle.

- Multi-user access: invite, accept, remove (see
  ENVELOPE_ENCRYPTION_DESIGN.md sections 5 and 6)
- KEK rotation CLI (`blu kek rotate`, `blu kek status`)
- Recovery kit generation (`blu recovery-kit generate`, optional PDF)

## v0.3.0-alpha

Operational maturity.

- Additional storage backends (DigitalOcean Spaces, GCS, Azure Blob)
- Streaming index I/O (memory-mapped or streaming reads instead of
  loading full index into memory)
- Event collision handling (delete from one backend, sync from another
  where data is still active)
- `--verbose` option for `list-files` (chunk count, size, encryption
  status)
- Color output

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
- Backward compatibility with pre-v0.1.0 formats
- Windows support (macOS primary, Linux secondary)
- GUI application
