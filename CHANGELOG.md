# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
- Shared push path (`sync`, `add`, `delete-files`, tags, defrag) always
  fetch+merges before upload

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
