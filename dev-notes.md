# dev notes

## TODO

- [ ] add a --verbose option to `list_files` which will show number of chunks a
  file has been broken into and the chunk size, maybe also whether it's been
  encrypted or not (but really this just depends on if a blob index exists ...)

- [ ] add to Hash type and allow for different multihashes?
  -- thinking on this, multihash is really just hash digest itself + type of hash algo

- [ ] upgrade multihash crate (a bit of work for v0.19, maybe an hour or 2)

- [ ] tokio for async

- [x] Remove "./" prefix from indexes / all paths.
    - done, 2023-07-04

- [ ] Add and start to maintain a [changelog](https://keepachangelog.com/en/1.1.0/)
  - Yes, even now. For the changes below that are to-DONE, but I need/want to keep a record of it

-- STREAM INDEXING TO DISK, DO NOT KEEP IT ALL IN MEMORY ... or do?
  - memory map it?

-- done, 2023-01-14
- [x] restore (functionality / util) from given datadir + indexes
  - [x] implement as separate util in src/bin/

- [ ] filename search
  -- consider https://github.com/BurntSushi/suffix for this

- [x] split blob index + blob buffer out from BlobManager
  - [x] blob index
    - [x] implement
    - [ ] test
  - [x] blob buffer refactor
  -- maps plain to encrypted -- built when files are encrypted and STREAMs the plain text thru to the encrypted data store -- be that local or s3/

- [ ] status command
  -- which does what? Describe this.
  -- Could display files which are in the PlainIndex but not encrypted
  -- Could display stats, e.g. # files, # bytes de-duplicated (saved), x tags being used, etc.

- [x] Implement document index conceptually separate from encryption/hash index
  - [/] search tags / filenames
  - [x] tag/untag files
  - [x] list all tags
  - [ ] add/edit/remove notes on files, larger bodies of text than tag. Should also be searchable.

- [x] 2022-03-22: block-level de-duplication -- v0.2.x branch is dedicated to this. I'm convinced this is the way forward.
  - [x] 2022-05-07: This is mostly done, blob index and manager are finished. Just need to...
  - [x] 2022-05-07: Add encryption (and possibly compression) to the blob before hashing/writing.
  - [x] 2022-05-07: Add tests for BlobManager. Lots of tests. (won't do, this has been refactored away)
    - [x] 2023-01-05: Reconsider this design (refactored)

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

Add passphrase encryption for the on-disk private key storage, which must be unlocked before Blu can decrypt antyhing.

Priv key never leaves device (not in sync dir).

### Search - Document Index

There should be a search function which searches filenames, tags, notes and returns most (or even all) relevant matches (tweakable of course).

### Initialization

```sh
blu init /  # should not be allowed
# <== root filesystem backups are not supported due to the size and amount of OS backups, please extra space is needed for encryption and de-duplication. Please use a custom directory for specific files

blu init .  ./data  # should not be allowed
# <== please use only 1 directory for a blu installation (you can have multiple, but they will need to be managed separately)
```

## Web UI

If a web-ui is added, probably would like to use Actix-Web. A new version was just released recently (as of 2022-02-27).
- note (2023-04-14): Might consider Axum instead, will have to evaluate.


## TODOs

- Should there be a .bluignore, similar to .gitignore? Or within .blu, e.g. .blu/ignore?

- Consider licensing as Apache + MIT dual license or similar

### Old notes from main binary (pre-v0.2):

// There are 2 operations:
//     a. archive - encrypt+de-duplicate new files
//     b. restore - restore from backup
//
// now, difference method depends on the operation...
//
// if we are doing in archive (encrypted any new files), then we want to get
// the difference of:
//
// index - enc_idx
// ... ignoring any extra encrypted files lying around.
//
// Likewise, a restore operation would be the opposite.
// enc_idx - index
// ... restore any left over, ignoring un-encrypted files lying around.
