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
- [ ] Backend `list` API for orphan blob discovery

## Data Management

- [ ] Event collision handling (e.g. user deletes from one backend,
      syncs from another where data is still active; consider event
      sourcing pattern)

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

Related doctor follow-up (needs storage API first):

- [ ] Backend `list` + orphan blob detection / repair in `blu doctor`
      (same item as under Storage Backends)

## UX

- [x] `.bluignore` file (similar to `.gitignore`)
- [x] `blu doctor` diagnostics
- [ ] `--verbose` option for `list-files` (show chunk count, chunk
      size, encryption status)
- [ ] Add/edit/remove notes on files (larger text bodies than tags,
      searchable)
- [ ] Color output

## Ideas (Low Priority)

- [ ] Configurable hashing algorithm (with backward compat: old
      hashes compared using the algorithm that produced them)
- [ ] Global hash table with multihash support (integer IDs mapping
      to multihash arrays for smaller indexes and algorithm agility)
- [ ] Web UI for browsing vaults
- [ ] Hardware key support (YubiKey/Ledger for UK storage)
- [ ] Vault sharing via URL (`blu://vault/s3:bucket:prefix?invite=...`)
