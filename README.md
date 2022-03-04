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

### Restore

## License

[ISC](LICENSE)
