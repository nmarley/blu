# blu - encrypted and de-duplicated file archival system, written in Rust

> "Not your keys, not your secrets ..."

---

**Open Source Release**

This project was born from a compelling insight shared by Balaji in an interview: someday the "cloud will burst," meaning state actors could potentially access any secrets stored in traditional cloud services like S3 or Google Drive. True privacy requires encrypting your data with keys that you--and only you--control.

While I initially explored commercial applications for this, I believe the most impactful path forward is thru open source. Rather than keep this locked away (and collecting... ahem, Rust...), I'm excited to open it up as both a working solution and a demonstration of what's possible when we prioritize user-controlled encryption. The core functionality is solid, and I believe this project can serve as a foundation for others to build upon, whether for personal use, enterprise applications, or further research into decentralized + encrypted storage solutions.

---

Based on directories in the typical \*nix hierarchical file system (HFS), this will read all files in the directory, and encrypt, de-duplicate and archive to any of several configurable backends, including locally and cloud object storage such as Amazon s3.

All encryption in the project uses [rage](https://github.com/str4d/rage), based on age by [@FiloSottile](https://twitter.com/FiloSottile) and [@Benjojo12](https://twitter.com/Benjojo12).

## Features

- **Encryption-Centric Design**: Developed with the premise of "own your encryption keys", ensuring data privacy against potential cloud breaches.
- **Cryptographic Hashing**: Files are uniquely identified using cryptographic hashes rather than filenames, enhancing data integrity and security.
- **Intelligent De-duplication**: Implemented chunking to de-duplicate files based on contiguous byte sequences, optimizing storage efficiency.
- **Robust Encryption**: Utilizes the age encryption scheme with age keys (X25519) for reliable asymmetric encryption.
- **Storage Flexibility**: Equipped with a modular backend, supporting local filesystem and Amazon S3.
- **Comprehensive Metadata Handling**: Stores plaintext metadata, including filenames and tags, locally. Metadata uploads are encrypted to ensure confidentiality.
- **Integrated Tagging System**: Includes a tagging system and tag index, allowing users to organize and locate their data efficiently.
- **Remote Index Sync**: Push and pull encrypted indexes to/from the backend, enabling access from multiple machines.

## Quick Start

### Initialize a new vault

```sh
# Create a new blu vault with passphrase-protected key
blu init /path/to/your/data

# Or without passphrase (for automation, not recommended for sensitive data)
blu init --no-passphrase /path/to/your/data
```

### Sync files (add + encrypt)

```sh
# Sync all files in the vault directory
blu sync

# Sync specific paths
blu sync ./documents ./photos

# Sync and push indexes to remote backend
blu sync --push
```

### List files

```sh
# List all indexed files
blu ls

# With filter
blu list-files --filter "*.pdf"
```

### Restore files

```sh
# Restore files by path pattern
blu restore-files --path "photos/*.jpg" --to /tmp/restored

# Restore all files
blu restore-files --all --to /tmp/restored

# Restore by hash prefix
blu restore-files --file-hashes abc123
```

### Remote Index Sync

```sh
# Push indexes to remote after sync
blu sync --push

# Pull indexes from remote (e.g., on a different machine)
blu pull --force
```

### Search

```sh
# Search for files by name or tag
blu search passport
```

## Configuration

The configuration is stored in `.blu/config.toml`:

```toml
blu_version = "0.5.0"

[encryption]
recipient = "age1..."  # Your public key

[backend]
type = "local"
path = ".blu/data"

# Or for S3:
# [backend]
# type = "s3"
# bucket = "my-bucket"
# prefix = "backups/photos"
# region = "us-east-1"
```

## S3 Backend

To use S3 as a backend, edit your config:

```toml
[backend]
type = "s3"
bucket = "your-bucket-name"
prefix = "optional/prefix"
region = "us-east-1"
```

AWS credentials are loaded from the environment (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`) or IAM roles.

## Security

- **Key Management**: Your private key lives at `~/.blu/identity.age` (one copy, shared across all vaults). Back it up securely! Without it, your data cannot be decrypted.
- **Passphrase Protection**: By default, your private key is encrypted with a passphrase. Use `--no-passphrase` only for automation scenarios.
- **No Key Escrow**: blu never stores or transmits your keys. You are solely responsible for key backup.

## Commands Reference

| Command | Description |
|---------|-------------|
| `init` | Initialize a new blu vault |
| `sync` | Add files and encrypt (combines add + encrypt-files) |
| `ls` / `list-files` | List indexed files |
| `restore-files` | Restore files from encrypted archive |
| `pull` | Pull indexes from remote backend |
| `search` | Search files by name or tag |
| `status` | Show vault status |
| `add` | Add files to index (plumbing) |
| `encrypt-files` | Encrypt indexed files (plumbing) |
| `tagger` | Manage tags on files |

## Global Options

| Option | Description |
|--------|-------------|
| `--bludir <path>` | Target folder for blu to operate in (like `git -C`) |
| `--no-passphrase` | Don't prompt for passphrase (fail if key is encrypted) |

## Building from Source

```sh
cargo build --release
```

## Running Tests

```sh
cargo test
```

## License

This project is licensed under either of

 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   https://opensource.org/licenses/MIT)
 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   https://www.apache.org/licenses/LICENSE-2.0)
