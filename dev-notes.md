# dev notes

## Roadmap

### Vision

An encrypted and de-duplicated file archival system, written in Rust. This project was inspired by balaji's comment in an interview that someday the "cloud will burst", meaning some state actor could leak any/all secrets stored on the cloud, in S3/Google Drive, etc. Nothing is secret/private if you aren't encrypting your data w/your own keys that you control.

Note on encryption(2024-02-04): Ideally this will be future proof and therefore quantum-resistant, so once a reliable Kyber implementation exists in Rust, I would like to move to that. I will be closely following Filippo Valsorda's (author of age) work in the space and probably using whatever he recommends. He himself is waiting for a NIST standard to emerge so hopefully that will be settled on soon-ish (mid-late 2024?).

### Project Guidelines

- The CLI should only use `--long-args` and not short ones like `-c`.

### Milestones

Milestone: (Q4, 2023)

- Multi-key encryption/recovery (envelope encryption scheme w/DEKs, KEKs)
    See also:
    - <https://cloud.google.com/kms/docs/envelope-encryption>
    - <https://stackoverflow.com/questions/72570215/data-encryption-keys-and-key-encryption-keys>

- Support for Cloud Backends
    - [x] S3
    - [ ] Google Cloud Storage
    - [ ] Azure Blob Storage
    - [ ] Digital Ocean
- Multi-backend support

Hopefully also (might be a bit much):

- Full search index for file paths and tags (not data itself)
- Seed Phrase generation / recovery for AGE keys + Recovery Kits (a la 1Password)

Milestone: (Q1, 2024)

- UI - likely web based, axum or actix
- Static site for project created + online
- Add changelog / public release -- at this point all previous history will be squashed and archived away in a private repo
- Async io for restore/encryption + benchmarks vs non-async


## Understand

Uses <https://crates.io/crates/age> for encryption.

See also: [https://rust-cli.github.io/book/index.html](Command line apps in Rust).

Clap: <https://docs.rs/clap/latest/clap/>

Multihash for hashing <https://github.com/multiformats/rust-multihash>
- All hashes used within the project should be multihashes.

## Design

### Key Init / Restore

Add 24-word seed phrase gen / recovery for AGE keys. This will be part of the recovery kit.

Add passphrase encryption for the on-disk private key storage, which must be unlocked before Blu can decrypt anything.

Priv key never leaves device (not in sync dir).

### Search - Document Index

There should be a search function which searches filenames, tags, notes and returns most (or even all) relevant matches (tweakable of course).

### Initialization

```sh
blu init /  # should not be allowed
# <== root filesystem backups are not supported due to the size of OS backups, plus extra space is needed for encryption and de-duplication. Please use a standalone directory as the blu vault.

blu init .  ./data  # should not be allowed
# <== please use only 1 directory for a blu installation (you can have multiple, but they will need to be managed separately)
```

## Web UI

If a web-ui is added, probably would like to use Actix-Web. A new version was just released recently (as of 2022-02-27).
- note (2023-04-14): Might consider Axum instead, will have to evaluate.
