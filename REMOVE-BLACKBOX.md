# Remove BlackBox: Analysis and Plan

## Why BlackBox no longer fits

BlackBox was designed for v1, where it was literally a black box: throw
plaintext in, get ciphertext out. The caller did not care whether crypto
happened locally or in the agent. That was a clean abstraction when the
agent handled raw data.

After the v1 removal, BlackBox sits at the wrong abstraction level. Data
encryption is always local now (symmetric ChaCha20-Poly1305 with the
DEK). What varies between InProcess and Agent is only who provides the
DEK, that is, who wraps and unwraps it using the KEK.

The current Agent path in `encrypt_typed`:

1. `client.wrap_dek()` (agent generates DEK, wraps with KEK)
2. `dek.encrypt_data(data)` (local symmetric encryption)
3. `v2format::write_v2(...)` (local file assembly)

The InProcess path:

1. `v2format::encrypt_v2()` (generates DEK, wraps with KEK, encrypts,
   assembles; all local)

Both paths do the same thing. They differ only in where the KEK lives.
This is the standard KMS pattern (AWS KMS, GCP CMEK, Vault Transit): a
key management service that wraps/unwraps DEKs, with all bulk data
encryption happening locally.

## What is wrong with keeping BlackBox as-is

1. The `InProcess { identities }` field stores X25519 identities that
   are never used for data operations anymore. Dead weight.

2. The name "BlackBox" does not communicate what it actually does. It
   sounds like it handles encryption, but it only manages key material.

3. It conflates two concerns: key management (KEK/DEK wrapping) and data
   encryption (symmetric). In canonical envelope encryption, these are
   separate.

4. `passphrase_encrypt` and `passphrase_decrypt` are free functions
   sitting outside BlackBox, which is inconsistent but correctly so,
   since they are a different concern. This inconsistency highlights that
   BlackBox is not the right shape.

## What the clean replacement looks like

A trait called `DekProvider` with two implementations:

```rust
trait DekProvider {
    fn wrap_dek(&self) -> Result<(Dek, Vec<u8>, u16)>;
    fn unwrap_dek(&self, wrapped: &[u8], version: u16) -> Result<Dek>;
}

struct LocalKeyProvider {
    kek: Kek,
    kek_version: u16,
}

struct AgentKeyProvider {
    client: AgentClient,
}
```

Then encryption and decryption become straightforward free functions (or
a thin `Vault` struct) that accept a `&dyn DekProvider`:

```rust
fn encrypt_blob(data: &[u8], keys: &dyn DekProvider) -> Result<Vec<u8>> {
    let (dek, wrapped, version) = keys.wrap_dek()?;
    let payload = dek.encrypt_data(data)?;
    let mut out = Vec::new();
    v2format::write_v2(&mut out, FileType::Blob, version, &wrapped, &payload)?;
    Ok(out)
}

fn decrypt(data: &[u8], keys: &dyn DekProvider) -> Result<Vec<u8>> {
    let (header, offset) = v2format::read_header(data)?;
    let dek = keys.unwrap_dek(&header.wrapped_dek, header.kek_version)?;
    dek.decrypt_data(&data[offset..])
}
```

## Migration scope

Every callsite that currently takes `&BlackBox` would take
`&dyn DekProvider` instead. The main touchpoints:

- `BlobBuffer::new` and `BlobBuffer` internals (`src/blob.rs`)
- `Config::load_plain_index`, `load_blob_index`, `load_tag_index` and
  their write counterparts (the `load_index!` / `write_index!` macros
  in `src/config.rs`)
- `BlackBoxSerializable` trait in `src/io.rs`
- `write_index_file` helper in `src/cli/mod.rs`
- `init_vault` in `src/cli/init.rs`
- Sync, encrypt-files, restore-files CLI commands
- Agent state (replaces `blackbox: Option<BlackBox>` with the KEK cache
  it already has; the agent already implements wrap/unwrap directly)

## What gets deleted

- `src/age.rs`: the entire `BlackBox` struct, `BlackBoxInner` enum,
  `KekContext`, and all methods on `BlackBox`
- `BlackBox::new()`, `BlackBox::from_agent()`, `with_kek()`, `set_kek()`
- The `age` crate dependency drops from data-path code (retained only
  for KEK wrapping in `kek.rs` and passphrase encryption in `age.rs`)
- `keys::blackbox_from_identity()` in `src/keys/mod.rs`

## What stays

- `passphrase_encrypt` / `passphrase_decrypt` (identity file encryption)
- `age::Encryptor` / `age::Decryptor` usage in `keys/kek.rs` (KEK
  wrapping with PQ recipients)
- `v2format.rs` (unchanged; already has the right shape)
- `Dek` and `Kek` types (unchanged)
