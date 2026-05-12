# Remove BlackBox: Implementation Plan

Replaces `BlackBox` with a `DekProvider` enum and free envelope
functions. Each stage is one atomic commit.

## Design decisions

- **Name:** `DekProvider`. Matches canonical KMS terminology for what
  the abstraction does (wrap/unwrap DEKs).
- **Concrete enum, not a trait.** Two variants (Local, Agent), single
  crate, no external consumers, no extensibility needed. Naturally
  `Clone`, `Send`, `Sync`, no `Box`/`dyn`/`Arc`, no lifetime friction.
- **Fix version validation, defer multi-KEK resolution.** The Local
  variant will validate that the requested KEK version matches the held
  version (currently InProcess silently ignores it). Multi-version KEK
  lookup via `KekStore` is a follow-up; the interface accommodates it
  via the `version: u16` parameter on `unwrap_dek`.
- **Top-level module.** `src/dek_provider.rs` at the crate root.
  `DekProvider` bridges `keys` and `agent`, so it belongs above both to
  preserve clean dependency layering: `cli` -> `dek_provider` ->
  (`keys`, `agent`).

## Stage 1: Introduce DekProvider enum and envelope functions

1a: Create `src/dek_provider.rs` with:

```rust
pub enum DekProvider {
    Local { kek: Kek, kek_version: u16 },
    Agent { client: AgentClient, kek_dir: Option<String> },
}
```

1b: Implement `wrap_dek(&self) -> Result<(Dek, Vec<u8>, u16)>` and
    `unwrap_dek(&self, wrapped: &[u8], version: u16) -> Result<Dek>`
    with version validation on both variants (error if requested version
    != held version).

1c: Implement `Clone` manually (Agent variant reconnects via
    `AgentClient::new()`, matching current BlackBox clone behavior).

1d: Add free functions `encrypt_envelope` and `decrypt_envelope`,
    pulling logic from `BlackBox::encrypt_typed` and `BlackBox::decrypt`.

1e: Register `mod dek_provider` in `src/lib.rs`.

1f: Add doc comments on all public items.

## Stage 2: Migrate the serialization layer

2a: Rename `BlackBoxSerializable` to `EncryptedSerializable` in
    `src/io.rs`. Change `write`/`read` signatures from `&BlackBox` to
    `&DekProvider`.

2b: Update `gen_std_bbserde!` macro to call
    `encrypt_envelope`/`decrypt_envelope`.

2c: `BlobIndex`, `PlainIndex`, `TagIndex` recompile through the macro;
    no manual changes needed on those types.

## Stage 3: Migrate blob read/write

3a: Change `BlobBuffer`'s `bbox: BlackBox` field to
    `keys: DekProvider` (owned, cloneable, no Box needed).

3b: Change `EncBlobReader`'s `bbox: &'a BlackBox` to
    `keys: &'a DekProvider`.

3c: Update `roll_new_blob()` to call `encrypt_envelope` and
    `get_bytes()` to call `decrypt_envelope`.

## Stage 4: Migrate Config index helpers

4a: Update `load_index!` macro parameter from `&BlackBox` to
    `&DekProvider`.

4b: Update `write_index!` macro parameter from `&BlackBox` to
    `&DekProvider`.

4c: All six generated `Config` methods change signature accordingly.

## Stage 5: Migrate CLI helpers and all commands

5a: Change `load_config_and_blackbox()` to `load_config_and_keys()` in
    `helpers.rs`. Return `(Config, DekProvider)`. Agent path returns
    `DekProvider::Agent { .. }`, init path returns
    `DekProvider::Local { .. }`.

5b: Update all 13 CLI command files: `add.rs`, `encrypt_files.rs`,
    `restore_files.rs`, `delete_files.rs`, `list_files.rs`, `status.rs`,
    `search.rs`, `tagger.rs`, `sync.rs`, `write_index.rs`,
    `read_index.rs`, `defrag_blobs.rs`, `backend_cmd.rs`.

5c: Update `write_index_file()` to take `&DekProvider`.

5d: Update `init_vault()` in `init.rs` to construct
    `DekProvider::Local { kek, kek_version: 0 }`.

5e: Update `pull.rs` (unused keys, but signature change needed).

## Stage 6: Delete BlackBox and clean up

6a: Delete `BlackBox`, `BlackBoxInner`, `KekContext`, all methods,
    `Default`/`Debug`/`Clone` impls from `src/age.rs`.

6b: Keep `passphrase_encrypt`/`passphrase_decrypt` in `src/age.rs`
    (they use the `age` crate for identity file protection; the module
    name is still accurate).

6c: Delete `src/agent/blackbox.rs` and remove `mod blackbox` from
    `src/agent/mod.rs`.

6d: Migrate the 7 unit tests from `age.rs` to `dek_provider.rs` as
    tests of `DekProvider::Local` + `encrypt_envelope`/`decrypt_envelope`.

6e: Rewrite the agent integration test from `agent/blackbox.rs` using
    `DekProvider::Agent`, place it as an inline test in
    `dek_provider.rs`.

6f: Run `cargo test`, `cargo clippy`, `cargo fmt -- --check`; fix
    anything that breaks.
