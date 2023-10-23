# blu - encrypted and de-duplicated file archival system, written in Rust

> "Not your keys, not your secrets ..."

Based on directories in the typical \*nix hierarchical file system (HFS), this will read all files in the directory, and encrypt, de-duplicate and archive to any of several configurable backends, including locally and cloud object storage such as Amazon s3.

All encryption in the project uses [rage](https://github.com/str4d/rage), based on age by [@FiloSottile](https://twitter.com/FiloSottile) and [@Benjojo12](https://twitter.com/Benjojo12).

## Encrypted & De-duplicated File Archival System in Rust

- Encryption-Centric Design: Developed with the premise of "own your encryption keys", ensuring data privacy against potential cloud breaches.
- Cryptographic Hashing: Files are uniquely identified using cryptographic hashes rather than filenames, enhancing data integrity and security.
- Intelligent De-duplication: Implemented chunking to de-duplicate files based on contiguous byte sequences, optimizing storage efficiency.
- Robust Encryption: Utilizes the age encryption scheme with age keys (related to ed25519) for reliable asymmetric encryption.
- Storage Flexibility: Equipped with a modular backend, currently featuring an S3 adapter for cloud storage.
- Comprehensive Metadata Handling: Stores plaintext metadata, including filenames and tags, locally. Metadata uploads are encrypted to ensure confidentiality.
- Integrated Tagging System: Includes a tagging system and tag index, allowing users to organize and locate their data efficiently.

## Usage

### Init

```sh
blu init .
```

### Config

```sh
vi .blu/config.json
```

### Add

Single file w/optional tags:

```sh
blu add ./passport.png --tags passport,US,Alice
```

Entire dir:

```sh
blu add ./
```

### Restore

```sh
blu restore .
```

### Search for all files w/tag? (combine w/the query command below):

```sh
blu search --tag iptu
blu search --tags passport,John,fra

blu query --tags passport,US,Alice
```

### Tags

Add tag 'datasheet' to all files in /data/datasheets. Should not tag or add
files which are not yet indexed.

```sh
blu tag --add --tag datasheet ./data/datasheets
```

Some examples:

```sh
# add `datasheet` tag to everything in ./data/hw-ds
cargo run --bin tagger -- --tags datasheet ./data/hw-ds

# manip tags for file `./data/hw-ds/MQ4.pdf` : add `sensor` tag, remove `fart` tag
cargo run --bin tagger -- --tags sensor,:fart ./data/hw-ds/MQ4.pdf

# manip tags for file `./data/hw-ds/MQ4.pdf`
#      adds:  `sensor`, `fart-detector`, `methane`
#   removes:  `hello`, `silly`, `fart`
cargo run --bin tagger -- --tags sensor,fart-detector,methane,:hello,:silly,:fart ./data/hw-ds/MQ4.pdf

# specify file hash to tag
cargo run --bin tagger -- --data-hash-filter 1bfdefb1375aa14 --tags pinout,stm32,black-pill,:datasheet ./data/hw-ds
```

### TagSpec

Tagspec is just a way of passing any number of tags to an operation, as well as
whether to remove or add any specific given tags. It consists of a string of
comma-separated tags, ideally sanitized/normalized (but the tools should be
written to sanitize all tag inputs regardless). If a tag has a leading colon
character (`:`), it indicates that the tag should be removed instead of added.

## License

[ISC](LICENSE)
