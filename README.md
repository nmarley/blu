# blu - encrypted and de-duplicated file archival system, written in Rust

> "Not your keys, not your secrets ..."

Based on directories in the typical \*nix hierarchical file system (HFS), this will read all files in the directory, and encrypt, de-duplicate and archive to any of several configurable backends, including locally and cloud object storage such as Amazon s3.

All encryption in the project uses [rage](https://github.com/str4d/rage), based on age by [@FiloSottile](https://twitter.com/FiloSottile) and [@Benjojo12](https://twitter.com/Benjojo12).

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

Note that tags are processed in the order they are received in the tagspec. For example, the argument `--tags hello,world,:hello` will add tag `hello`, the `world,` then remove the `hello` tag, resulting in the tagged object NOT having the `hello` tag at the end of the command execution.

## License

[ISC](LICENSE)
