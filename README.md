# blu

Encrypted, deduplicated file archival CLI written in Rust.

> "Not your keys, not your secrets ..."

**Status:** 0.7.x pre-release (dogfood / late-alpha quality). Breaking
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
- **Multi-device catalog merge**: content-hash union on pull/push so either
  machine can add or delete; last-write-wins tombstones for deletes
- **Agent daemon**: unlock once, zeroize on lock; biometric gate on macOS
- **`blu serve`**: S3-compatible localhost API over the encrypted vault
- **`.bluignore`**: gitignore-style exclusion during backup/status walks
- **`blu doctor`**: vault health checks (config, keys, indexes, blobs)

## Product model

blu is an encrypted, content-addressed **vault**:

- Remote backend holds the shared **catalog** (encrypted indexes) and
  opaque **blobs**
- Local directory is an optional **checkout** of plaintext, not a
  Dropbox-style sync folder
- Multi-device: same identity + backend; publish, pull catalog, restore
  what you need

See `docs/design/CLI_UX.md` for vocabulary and invariants.

## Quick start

```sh
# 1. Create a global identity (once per machine / user)
blu identity init

# 2. Unlock the agent (Touch ID or passphrase)
blu unlock

# 3. Initialize a vault in a directory
blu init ~/Archives/photos

# 4. Copy or create files under the vault, then publish
cd ~/Archives/photos
blu backup

# 5. Inspect
blu status
blu ls
blu doctor
```

Optional: put a `.bluignore` at the vault root (gitignore syntax). The
`.blu/` and `.git/` directories are always skipped.

### Restore files (materialize plaintext)

`blu pull` and `blu open` update the **catalog** only. They do not write
plaintext into the vault working tree. Use `restore` to decrypt:

```sh
blu restore --path "photos/*.jpg" --to /tmp/restored
blu restore --all --to /tmp/restored
blu restore --file-hashes abc123
```

### Multi-device (same identity, shared backend)

Git-like for the catalog, deliberate for plaintext checkout:

| Command | Role |
|---------|------|
| `blu backup [paths]` | Index local paths, encrypt, merge remote indexes, push |
| `blu pull` | Fetch remote indexes and **union-merge** into local (catalog only) |
| `blu pull --force` | Hard reset: discard local indexes, take remote only |
| `blu status` | Working tree vs catalog vs remote |
| `blu ls` | List what the **catalog** knows about (not directory listing) |
| `blu restore` | Materialize plaintext from catalog + blobs |
| `blu rm` | Tombstone + cascade (multi-device safe) |

Day-to-day:

```sh
# Machine A: publish
blu backup path/or/.

# Machine B: refresh catalog, then checkout deliberately
blu pull
blu status
blu restore --path 'music/*'   # or --all when intentional
```

Either machine may add files and `backup`. The other machine `pull`s and
sees the union in `blu ls`. Concurrent adds merge by content hash.
Deletes record tombstones so a stale peer does not reanimate a removed
file; re-adding the same content after a delete wins when the re-add is
newer.

Path conflict: the same path maps to two different content hashes after
a merge (both versions stay in the index). `blu pull` prints a warning;
restore by hash or resolve paths manually.

Fresh machine (disaster recovery or second computer):

```sh
# 1. Recover identity from the 24-word mnemonic
blu identity recover

# 2. Open the vault from S3 (writes local .blu/, pulls keys + indexes)
blu open --type s3 \
  --bucket my-bucket \
  --prefix backups/photos \
  --region us-east-1 \
  --dir ~/Archives/photos

# 3. Unlock, inspect catalog, restore plaintext where you want it
cd ~/Archives/photos
blu unlock
blu ls
blu restore --all --to /tmp/restored
```

Existing local vault, refresh from backend:

```sh
blu pull              # merge remote into local (catalog only)
# blu pull --force    # only if you intend to drop local-only index state
blu ls
blu restore --all --to /tmp/restored
```

If the backend was created before KEK push existed, run any index-pushing
command once on the **original** machine (for example `blu backup`) so
`keys/kek.toml` is published, then retry `blu open`.

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

### S3 Intelligent-Tiering (cold media vaults)

Blob objects are uploaded as `INTELLIGENT_TIERING` with tag
`blu-role=blob`. Indexes and keys stay `STANDARD` (`blu-role=catalog`).
Apply a one-time bucket Intelligent-Tiering rule so blobs enter Deep
Archive Access after 365 days of no access:

```sh
blu backend intelligent-tiering print > blu-it-config.json
aws s3api put-bucket-intelligent-tiering-configuration \
  --bucket my-bucket \
  --id blu-blobs-deep-archive \
  --intelligent-tiering-configuration file://blu-it-config.json
```

When blobs are in an archive tier, restore is async:

```sh
blu thaw --path 'photos/**'          # start RestoreObject
blu thaw --status                    # poll progress
blu restore --path 'photos/**' --thaw
```

See `docs/design/S3_COLD_STORAGE_DESIGN.md` for the full model.

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
| `backup` | Index paths, encrypt, merge remote indexes, push |
| `pull` | Merge remote catalog (default); `--force` resets to remote |
| `status` | Working tree vs catalog vs remote |
| `doctor` | Vault health diagnostics (includes cold-storage checks) |
| `thaw` | Initiate / status S3 archive restores for vault blobs |
| `restore` | Materialize plaintext (`--thaw` / `--wait` for cold blobs) |
| `ls` / `list-files` | List catalog entries |
| `search` | Search filenames and tags |
| `rm` | Tombstone + cascade blobs; multi-device safe |
| `defrag-blobs` | Repack partially-dead blobs; `--upgrade-format` for v2→v3 |
| `tagger` | Add/remove tags |
| `backend` | add / list / remove / set-default / rename / mirror / diff / intelligent-tiering print |
| `serve` | Localhost S3-compatible API |

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

- `docs/design/CLI_UX.md` — git-like vault CLI model
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
