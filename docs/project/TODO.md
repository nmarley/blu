# TODO

Consolidated backlog for blu. See ROADMAP.md for the sequenced milestone plan.

## Release Prep

- [x] Set up GitHub Actions CI (cargo build + cargo test on push)
- [x] Update README with current features, commands, config examples
- [x] Start maintaining a [changelog](https://keepachangelog.com/en/1.1.0/)
- [ ] Draft initial intro / release post
- [ ] Add CI/build badges and header image to README

## Crypto / Key Management

- [ ] Multi-user access: invite, accept, remove (see
      `docs/design/ENVELOPE_ENCRYPTION_DESIGN.md`)
- [ ] KEK rotation CLI (`blu kek rotate`, `blu kek status`)
- [ ] Recovery kit: `blu recovery-kit generate` with optional PDF export

## Storage Backends

- [ ] Additional storage backends: DigitalOcean Spaces, Google Cloud
      Storage, Azure Blob Storage
- [x] Backend `list_blob_paths` API (local walk + S3 ListObjectsV2;
      skips `indexes/` and `keys/`)

## Data Management

- [x] Multi-device index merge on pull/push (content-hash union; concurrent
      adds)
- [x] Delete tombstones with LWW re-add (plain index `deleted_files` /
      `file_times`) for multi-device delete propagation
- [x] Doctor orphan blob detection (`blob-orphans` warn via
      `list_blob_paths` vs `BlobIndex::path_index`)
- [ ] Tombstone GC / compaction (drop ancient tombstones after retention)
- [ ] Orphan blob reclaim (delete backend objects reported by
      `blob-orphans`): dry-run first, then explicit destroy command;
      never auto-delete on doctor
- [ ] Multi-device-safe blob GC: do not delete backend blob objects while
      another peer may still reference a shared chunk (grace period,
      refcount, or tombstone-first / GC-later). Required before any
      automatic or default-on orphan reclaim
- [ ] Optional user-facing `backend list-blobs` (or similar) plumbing
      CLI for operators; doctor already uses the storage API
- [ ] Event log / snapshot history if richer than LWW tombstones is needed

## Architecture

- [ ] Separate std/fs implementation from core API (accept bytes
      instead of filenames in lib, keep fs operations in tools layer)
- [ ] Streaming index I/O instead of loading full index into memory
      (memory-mapped files or streaming reads)

## `blu serve` (deferred from hardening)

Correctness and crypto bugs from the serve review are fixed. These
remain open by design:

- [ ] v2 vs v3 random-access latency benchmarks (prefix-fetch path is
      correct and tested; no measured numbers yet)
- [ ] Delta sync on returning machines (`sync_from_backend` currently
      does a full upsert re-populate of redb; compute/apply deltas only)
- [ ] Encrypt `.blu/serve.redb` at rest (today it holds plaintext index
      state on the local disk; accepted tradeoff for now)
- [ ] Crash-atomic index-push WAL (debounced flush can leave a window
      between blob upload/redb commit and encrypted index push to the
      backend)

Related doctor follow-up:

- [x] Backend `list` + orphan blob detection in `blu doctor`
      (warn-only; reclaim/repair still under Data Management)

## UX

- [x] `.bluignore` file (similar to `.gitignore`)
- [x] `blu doctor` diagnostics
- [ ] `--verbose` option for `list-files` (show chunk count, chunk
      size, encryption status)
- [ ] Add/edit/remove notes on files (larger text bodies than tags,
      searchable)
- [ ] Color output

## Ideas (Low Priority)

- [ ] Global hash table (integer IDs mapping to multihashes for
      smaller indexes)
- [ ] Web UI for browsing vaults
- [ ] Hardware key support (YubiKey/Ledger for UK storage)
- [ ] Vault sharing via URL (`blu://vault/s3:bucket:prefix?invite=...`)
