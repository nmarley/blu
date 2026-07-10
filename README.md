# blu

Encrypted, deduplicated file archival CLI written in Rust.

> "Not your keys, not your secrets ..."

**Status:** 0.7.0 pre-release (dogfood / late-alpha quality). Breaking
changes are expected.

## Why

Cloud storage is convenient until someone else can read it. blu encrypts
your files with keys only you control, deduplicates content-addressed
chunks, and stores opaque blobs on local disk or Amazon S3. A local agent
can cache unlock state (with macOS Touch ID when available).

## Features

- **Post-quantum hybrid identity**: BIP39 24-word mnemonic derives an
  ML-KEM-768 + X25519 user key (`age1pq...`)
- **Envelope encryption**: UK wraps KEK (age asymmetric); KEK wraps
  per-blob DEKs; bulk data is ChaCha20-Poly1305
- **v3 segmented AEAD blobs**: fixed-size segments with prefix-fetch
  reads (v2 still readable; upgrade via `defrag-blobs --upgrade-format`)
- **Content-addressed storage**: chunk + multihash dedup across files
- **Named multi-backend config**: local and S3, with mirror/diff
- **Agent daemon**: unlock once, zeroize on lock; biometric gate on macOS
- **`blu serve`**: S3-compatible localhost API over the encrypted vault
- **`.bluignore`**: gitignore-style exclusion during add/sync/status
- **`blu doctor`**: vault health checks (config, keys, indexes, blobs)

## Quick start

```sh
# 1. Create a global identity (once per machine / user)
blu identity init

# 2. Unlock the agent (Touch ID or passphrase)
blu unlock

# 3. Initialize a vault in a directory
blu init ~/Archives/photos

# 4. Copy or create files under the vault, then sync
cd ~/Archives/photos
blu sync

# 5. Inspect
blu status
blu ls
blu doctor
```

Optional: put a `.bluignore` at the vault root (gitignore syntax). The
`.blu/` and `.git/` directories are always skipped.

### Restore files (same machine)

```sh
blu restore-files --path "photos/*.jpg" --to /tmp/restored
blu restore-files --all --to /tmp/restored
blu restore-files --file-hashes abc123
```

### Restore on a new machine

Vault-changing commands push encrypted indexes **and** the UK-wrapped
KEK store to the backend. On a fresh machine you need only your
mnemonic, backend location, and AWS credentials:

```sh
# 1. Recover identity from the 24-word mnemonic
blu identity recover

# 2. Open the vault from S3 (writes local .blu/, pulls keys + indexes)
blu open --type s3 \
  --bucket my-bucket \
  --prefix backups/photos \
  --region us-east-1 \
  --dir ~/Archives/photos

# 3. Unlock and restore
cd ~/Archives/photos
blu unlock
blu ls
blu restore-files --all --to /tmp/restored
```

If the backend was created before KEK push existed, run any index-pushing
command once on the **original** machine (for example `blu sync`) so
`keys/kek.toml` is published, then retry `blu open`.

### Pull indexes (existing local vault)

```sh
# Indexes and KEK store are pushed after vault-changing commands.
blu pull --force
```

### Local S3-compatible API

```sh
blu serve --bind 127.0.0.1:7777
# Point any S3 client at http://127.0.0.1:7777
```

## Configuration

Vault config lives at `.blu/config.toml` (created by `blu init`):

```toml
blu_version = "0.7.0"
default_backend = "default"

[encryption]
pq_recipient = "age1pq..."  # your post-quantum hybrid public key

[backends.default]
type = "local"
path = ".blu/data"

# Example additional backend:
# [backends.s3-prod]
# type = "s3"
# bucket = "my-bucket"
# prefix = "backups/photos"
# region = "us-east-1"
```

AWS credentials for S3 come from the environment
(`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`) or IAM roles, not from
the config file.

```sh
blu backend add cold --type s3 --bucket my-bucket --region us-east-1
blu backend mirror --from default --to cold
blu backend diff --from default --to cold
```

## Security model

| Layer | Mechanism |
|-------|-----------|
| Identity | 24-word BIP39 mnemonic → PQ hybrid UK (ML-KEM-768 + X25519) |
| Identity at rest | `$XDG_DATA_HOME/blu/identity.age` (age scrypt, N_log_n ≥ 18) |
| KEK | One per vault under `.blu/keys/`, age-wrapped to your PQ recipient; also pushed to the backend |
| DEK | One per blob/index; ChaCha20-Poly1305 bulk encryption |
| Indexes | gzip + CBOR (ciborium) + envelope encryption; pushed with the KEK store |

- Private key material never leaves your machine (or the agent process).
- The backend holds only ciphertext (blobs, indexes, UK-wrapped KEK).
- There is no key escrow. Lose the mnemonic, lose the data.

- macOS: agent can gate unlock with Touch ID via Keychain.
- Linux: passphrase / mnemonic only (no biometric).

## Commands

| Command | Description |
|---------|-------------|
| `identity init` / `show` / `recover` | Global BIP39 identity |
| `unlock` / `lock` | Agent session |
| `agent status` / `stop` | Agent daemon control |
| `init` | Create a vault |
| `open` | Open an existing vault from a backend |
| `sync` | Add paths and encrypt |
| `status` | Working-tree vs index |
| `doctor` | Vault health diagnostics |
| `ls` / `list-files` | List indexed files |
| `search` | Search filenames and tags |
| `restore-files` | Decrypt files out of the vault |
| `delete-files` | Remove from index and cascade blobs |
| `defrag-blobs` | Repack partially-dead blobs; `--upgrade-format` for v2→v3 |
| `pull` | Pull indexes from a backend |
| `tagger` | Add/remove tags |
| `backend` | add / list / remove / set-default / mirror / diff |
| `serve` | Localhost S3-compatible API |
| `add` | Index only (no encrypt) |

Global options: `--bludir <path>` (like `git -C`), `--no-passphrase`.

## Build and test

```sh
cargo build --release   # binary: target/release/blu
cargo test
cargo clippy
cargo fmt -- --check

# Install into ~/.cargo/bin (re-ad-hoc-codesigns on macOS)
bash scripts/install-local.sh
```

`install-local.sh` is `cargo install --path . --force` plus, on Darwin,
`codesign -s -` so a fresh install is not SIGKILL'd by taskgated for an
invalid linker-signed copy. Ensure `~/.cargo/bin` is on your `PATH`.

CI runs on `macos-15` and `ubuntu-24.04` (build, test, clippy, fmt).

## Design docs

- `docs/design/ENVELOPE_ENCRYPTION_DESIGN.md` — key hierarchy
- `docs/design/BLU_SERVE_DESIGN.md` — `blu serve` architecture
- `docs/project/START-HERE.md` — living project status
- `docs/project/ROADMAP.md` — milestones
- `CHANGELOG.md` — release notes

## License

Licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  https://opensource.org/licenses/MIT)
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  https://www.apache.org/licenses/LICENSE-2.0)

at your option.
