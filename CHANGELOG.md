# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Breaking:** remove hidden `blu add`. Catalog-only publish is gone;
  use `blu backup` to index, encrypt, and publish. Push refuses when
  plain-index chunks lack ciphertext (`ensure_encryption_coverage`).
- `blu restore` fails closed: missing ciphertext skips the file before
  creating a dest; mid-write failures unlink partials and hard-error;
  each chunk is size+hash verified and the whole file hash is checked.
- `blu doctor` `encryption-coverage` is a failure (not a warning) when
  plain-index chunks lack blob-index ciphertext.

## [0.7.5] - 2026-07-11

### Added

- Design doc `docs/design/CLI_UX.md` for the git-like vault model
- `blu status` reports catalog vs remote (in sync / ahead / behind /
  diverged), checkout present vs missing, and unpublished local files
- `blu doctor` `catalog-remote` check warns when the local catalog is not
  fully on the remote
- Backend `list_blob_paths` (local + S3) for content-addressed blob
  objects; skips `indexes/` and `keys/`
- `blu doctor` `blob-orphans` check warns when backend objects are not
  referenced by the catalog (detect only; reclaim deferred)
- `blu backend rename` to rename a configured backend

### Changed

- **Breaking CLI rename (hard cut, no aliases):**
  - `sync` → `backup`
  - `restore-files` → `restore`
  - `delete-files` → `rm`
  - `add` is hidden plumbing (prefer `backup`)
- `blu pull` success copy is catalog-only and hints `blu restore` when
  checkout is incomplete
- Shared push path (`backup`, tags, defrag, `rm`) always fetch+merges
  before upload; push failure is a hard error
- Drop short flags for long-only CLI surface

## [0.7.4] - 2026-07-11

### Added

- Multi-device index sync: pull and push union-merge plain, blob, and tag
  indexes by content hash so concurrent adds from two machines survive
- Last-write-wins delete tombstones on the plain index (`file_times` /
  `deleted_files`) so multi-device deletes are not reanimated by a stale peer
- Push re-merges once when remote index ciphertext advances mid-push;
  fails with a clear error if the remote races again
- Multi-device smoke tests (sequential adds, concurrent adds, delete
  tombstone propagation) on a shared local backend

### Changed

- `blu pull` merges remote indexes into local by default (no longer
  requires `--force` for routine refresh)
- `blu pull --force` is a hard reset: discard local indexes and take the
  remote copy only
- Shared push path always fetch+merges before upload

### Dependencies

- Migrate crypto stack (ml-kem 0.3, sha2/sha3 0.11, chacha20poly1305 0.11,
  x25519-dalek 3, rand 0.10, bech32 0.12)
- Upgrade toml to 1.x and itertools to 0.15

## [0.7.3] - 2026-07-10

### Fixed

- Decrypt v3 segmented blobs in restore (v2 still works); fixes
  "unsupported format version: 3 (expected 2)" on fresh opens

### Added

- `bash scripts/install-local.sh` (`cargo install --path . --force` plus
  macOS ad-hoc codesign so taskgated does not SIGKILL installs)

## [0.7.2] - 2026-07-10

### Added

- `blu open` to bootstrap a vault from a backend on a new machine
- Push UK-wrapped KEK store to the backend with encrypted indexes
- Pull KEK store on open/pull so a new machine can unwrap with the mnemonic
- Document fresh-machine recovery (mnemonic, open, unlock, restore)

## [0.7.1] - 2026-07-10

### Changed

- User-global state moves from `~/.blu/` to XDG Base Directory paths on
  all platforms (including macOS). Defaults: config
  `~/.config/blu`, data `~/.local/share/blu`, state
  `~/.local/state/blu`, runtime `$XDG_RUNTIME_DIR/blu` (state-dir
  fallback when runtime is unset). Identity, agent socket/PID, and
  agent config are resolved via `src/user_paths.rs`. Vault-local
  `.blu/` is unchanged. No migration from `~/.blu/`; re-run
  `blu identity init` or recover if you still have files there.

## [0.7.0] - 2026-07-09

Pre-release dogfood surface after `v0.6.3`. Breaking changes remain expected.
Prior tags `v0.1.1` through `v0.6.3` exist without Keep a Changelog entries.

### Added

- v3 segmented AEAD blob format with prefix-fetch reads; v2 still readable;
  `defrag-blobs --upgrade-format` for migration
- `blu serve` local S3-compatible HTTP API over the encrypted vault
- `.bluignore` (gitignore-style) for add, sync, and status walks
- `blu doctor` vault health diagnostics
- Full delete cascade through the index and storage backend
- Blob defrag / repack with optional `--scrub`
- Vault summary on `status`
- End-to-end vault pipeline smoke tests
- GitHub Actions CI on `macos-15` and `ubuntu-24.04`
- Fast crate-vs-tag version check (`scripts/check-version.sh`, optional
  pre-push hook)

### Changed

- Index serialization uses CBOR via ciborium (not bincode)
- Scrypt work factor pinned to a minimum of 18 for identity files
- Plumbing commands (`write-index`, `encrypt-files`, `read-index`)
  hidden from help; obsolete `debug-index` removed
- Project docs reorganized under `docs/`

### Security

- v3 segment AAD binds header fields (segment size, count, plaintext
  length) in addition to the segment index
