# dev notes

## Roadmap

### Vision

An encrypted and de-duplicated file archival system, written in Rust. This project was inspired by balaji's comment in an interview that someday the "cloud will burst", meaning some state actor could leak any/all secrets stored on the cloud, in S3/Google Drive, etc. Nothing is secret/private if you aren't encrypting your data w/your own keys that you control.

### Milestones

Milestone: (Q3, 2023)

- full search index for file paths and tags (not data itself)

Milestone: (Q3, 2023)

- multi-key encryption/recovery

Milestone: (Q3, 2023)

- async io for restore/encryption + benchmarks vs non-async

Milestone: (Q4, 2023)

- Seed Phrase generation / recovery for AGE keys + Recovery Kits (a la 1Password)
- Support for Cloud Backends - s3, Google Cloud Storage, Azure Blob Storage, digital ocean

Milestone X: (Q4, 2023)

- Add changelog / public release -- at this point all previous history will be squashed and archived away in a private repo
- website / static site for project built

Milestone: (Q1, 2024)

- UI - likely web based, axum or actix

## Relevant notes

- I think I figured out the reason for the jumping all over the place -- the `encrypt_files` util (as of commit `f6f59ae4115a1e99788c80f512d9d295a59b6502`) is iterating over block index (not files index) without ever consulting the order of the files -- not doing it in order, so _that's_ why it's encrypting chunks in random order. By keying off `plain_index` and iterating the chunk hashes _first_, we should be able to then get the location from teh block index and then encrypt as usual, but this time in order, which should greatly speed up our restores. This + async threads should work much nicer/quicker.

## TODO

- [ ] add a --verbose option to `list_files` which will show number of chunks a
  file has been broken into and the chunk size, maybe also whether it's been
  encrypted or not (but really this just depends on if a blob index exists ...)

- [ ] add to Hash type and allow for different multihashes?
  -- thinking on this, multihash is really just hash digest itself + type of hash algo

- [ ] upgrade multihash crate (a bit of work for v0.19, maybe an hour or 2)

- [ ] tokio for async

- [ ] Add and start to maintain a [changelog](https://keepachangelog.com/en/1.1.0/)
  - Yes, even now. For the changes below that are to-DONE, but I need/want to keep a record of it

-- STREAM INDEXING TO DISK, DO NOT KEEP IT ALL IN MEMORY ... or do?
  - memory map it?

- [ ] status command
  -- which does what? Describe this.
  -- Could display files which are in the PlainIndex but not encrypted
  -- Could display stats, e.g. # files, # bytes de-duplicated (saved), x tags being used, etc.

- [ ] add/edit/remove notes on files, larger bodies of text than tag. Should also be searchable.

- [ ] Seed Phrase generation / recovery for AGE keys + Recovery Kits (a la 1Password)
  See: <https://electrum.readthedocs.io/en/latest/seedphrase.html>

- [ ] multi-key encryption/recovery. How to handle this?

Other storage backends such as s3, etc. Current version only implements local disk!
- [ ] s3
- [ ] digital ocean one?
- [ ] Google Cloud?
- [ ] Azure?

## Understand

Uses <https://crates.io/crates/age> for encryption.

See also: [https://rust-cli.github.io/book/index.html](Command line apps in Rust).

Clap: <https://docs.rs/clap/latest/clap/>

Multihash for hashing <https://github.com/multiformats/rust-multihash>
- All hashes used within the project should be multihashes.

Filemagic lib: <https://docs.rs/filemagic/0.12.3/filemagic/struct.Magic.html>

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


## TODOs

- Should there be a .bluignore, similar to .gitignore? Or within .blu, e.g. .blu/ignore?

- Consider licensing as Apache + MIT dual license or similar
