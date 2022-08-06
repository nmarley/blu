# dev notes

## TODO

- [ ] Implement document index conceptually separate from encryption/hash index
  - [ ] search tags / filenames
  - [ ] tag/untag files
  - [ ] list all tags

- [x] rename crate::block::File to something that does NOT conflict with std::fs::File

- [/] 2022-03-22: block-level de-duplication -- v0.2.x branch is dedicated to this. I'm convinced this is the way forward.
  - [x] 2022-05-07: This is mostly done, blob index and manager are finished. Just need to...
  - [ ] 2022-05-07: Add encryption (and possibly compression) to the blob before hashing/writing.
  - [ ] 2022-05-07: Add tests for BlobManager. Lots of tests.

- [ ] Seed Phrase generation / recovery for AGE keys + Recovery Kits (a la 1Password)
  See: <https://electrum.readthedocs.io/en/latest/seedphrase.html>

- [ ] Tags / notes on files (on the FileRef portion of the files index). Probably in a later release.

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

### Search

There should be a search function which searches filenames, tags, notes and returns most (or even all) relevant matches (tweakable of course).

```sh
blu init /  # should not be allowed
# <== root filesystem backups are not supported due to the size and amount of OS backups, please extra space is needed for encryption and de-duplication. Please use a custom directory for specific files

blu init .  ./data  # should not be allowed
# <== please use only 1 directory for a blu installation (you can have multiple, but they will need to be managed separately)
```

## Web UI

If a web-ui is added, probably would like to use Actix-Web. A new version was just released recently (as of 2022-02-27).


## TODOs

- Should there be a .bluignore, similar to .gitignore? Or within .blu, e.g. .blu/ignore?
