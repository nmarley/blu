# TODO

Consolidated backlog for blu.

## Release Prep

- [ ] Draft initial intro / release post
- [ ] Set up GitHub Actions CI (cargo build + cargo test on push)
- [ ] Update README with current features, commands, config examples
- [ ] Add CI/build badges and header image to README
- [ ] Crypto review (send Filippo an email?)
- [ ] Start maintaining a [changelog](https://keepachangelog.com/en/1.1.0/)

## Crypto / Key Management

- [ ] Multi-user access: invite, accept, remove (see
      ENVELOPE_ENCRYPTION_DESIGN.md sections 5 and 6)
- [ ] KEK rotation CLI (`blu kek rotate`, `blu kek status`)
- [ ] Recovery kit: `blu recovery-kit generate` with optional PDF export

## Storage Backends

- [ ] Additional storage backends: DigitalOcean Spaces, Google Cloud
      Storage, Azure Blob Storage

## Data Management

- [ ] Full data deletes (plain index deletes vs. full encrypted
      chunk deletes with blob marking)
- [ ] Blob defragmentation (reclaim space from deleted chunks by
      repacking remaining chunks into new blob files)
- [ ] Event collision handling (e.g. user deletes from one backend,
      syncs from another where data is still active; consider event
      sourcing pattern)

## Architecture

- [ ] Separate std/fs implementation from core API (accept bytes
      instead of filenames in lib, keep fs operations in tools layer)
- [ ] Streaming index I/O instead of loading full index into memory
      (memory-mapped files or streaming reads)

## UX

- [ ] `--verbose` option for `list-files` (show chunk count, chunk
      size, encryption status)
- [ ] Add/edit/remove notes on files (larger text bodies than tags,
      searchable)
- [ ] `.bluignore` file (similar to `.gitignore`)
- [ ] Color output
- [ ] `blu doctor` diagnostics

## Ideas (Low Priority)

- [ ] Configurable hashing algorithm (with backward compat: old
      hashes compared using the algorithm that produced them)
- [ ] Global hash table with multihash support (integer IDs mapping
      to multihash arrays for smaller indexes and algorithm agility)
- [ ] Web UI for browsing vaults
- [ ] Hardware key support (YubiKey/Ledger for UK storage)
- [ ] Vault sharing via URL (`blu://vault/s3:bucket:prefix?invite=...`)
