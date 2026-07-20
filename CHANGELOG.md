# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.0] - 2026-07-20

### Added

- S3 Intelligent-Tiering cold storage for vault blobs: blobs upload as
  `INTELLIGENT_TIERING` tagged `blu-role=blob`; catalog and keys stay
  `STANDARD` tagged `blu-role=catalog`
- `blu thaw` initiates and reports archive restores for a catalog
  selection (`--path`, `--file-hashes`, `--all`); `--wait` polls with
  exponential backoff (30s doubling to a 5min cap)
- `blu restore` cold handling: fails fast with a thaw hint on archived
  blobs; `--thaw` initiates restores; `--wait` blocks until readable
- `blu doctor` cold checks: `catalog-hot` (indexes/keys instantly
  readable), `blob-cold-status` (deterministic 64-blob sample),
  `bucket-it-config` (Deep Archive configuration present on the bucket)
- `blu backend intelligent-tiering print` emits the operator-applied
  bucket archive configuration; `--archive-days` adds an optional
  Archive Access tier before Deep Archive Access
- `blu serve` maps archived blob reads to S3 `InvalidObjectState` with
  a thaw hint; the GET preflight probes a file's blobs in parallel
- Backend `stat_object` / `restore_object` APIs and typed
  `BluError::ObjectArchived`
- Design doc `docs/design/S3_COLD_STORAGE_DESIGN.md`
- Scriptable passphrases: `BLU_PASSPHRASE` covers agent unlock paths and
  identity-file encryption, `BLU_MNEMONIC_PASSPHRASE` supplies the BIP39
  25th word, and `blu identity init --yes` skips interactive
  confirmations, so the whole pipeline (identity, init, backup, restore,
  doctor) runs headless
- `BLU_NO_BIOMETRIC` skips Touch ID keychain setup during
  `blu identity init` / `recover` for scripts and CI
- `scripts/e2e-passphrase-smoke.sh`: headless end-to-end smoke
  (sandboxed XDG dirs, encrypted-identity assertion, content diff)

### Changed

- **Breaking:** content hashes switch from SHA-512 to Blake3-256
  multihash; existing vaults must be re-initialized and re-backed-up
- **Breaking:** blobs move under the `blobs/` backend prefix
  (`blobs/d/dd4/...`); existing vaults must be recreated
- Bare `blu thaw --status` refuses full-index scans above 5,000 blobs;
  use `--all --status` or narrow the selection

## [0.7.6] - 2026-07-11

### Fixed

- `blu status` / `blu doctor` catalog-remote no longer report ahead when
  only index ciphertext differs (same logical catalog after re-encrypt).
  Tag content is compared after decrypt, not by ciphertext digest alone.
- Default `blu pull` (merge) preserves remote index ciphertext when the
  merge result matches remote, and keeps local ciphertext when it matches
  local. Only a true two-sided union re-encrypts. Noop pulls no longer
  forge false ahead.
- Fix clap panic on `blu backup --verbose` (global `-v` is a count; drop
  the conflicting bool subcommand flag and always print backup summary).

### Changed

- **Breaking:** remove hidden `blu add`. Catalog-only publish is gone;
  use `blu backup` to index, encrypt, and publish. Push refuses when
  plain-index chunks lack ciphertext (`ensure_encryption_coverage`).
- `blu restore` fails closed: missing ciphertext skips the file before
  creating a dest; mid-write failures unlink partials and hard-error;
  each chunk is size+hash verified and the whole file hash is checked.
- `blu doctor` `encryption-coverage` is a failure (not a warning) when
  plain-index chunks lack blob-index ciphertext.
- Default CLI log level is Warn (was Debug). `-v` enables info, `-vv`
  debug. `blu serve` and the agent daemon still log Info without `-v`.

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
