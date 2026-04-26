# PQ Integration Plan

Finishes the post-quantum integration for `blu init`. The crypto primitives are complete and tested. This plan wires them into the vault initialization flow.

## Problem

`cargo run init` only generates X25519 keys. The PQ stack (hybrid KEM, HPKE, age PQ recipient/identity, BIP39 derivation, KEK store) exists but is never invoked from the init command. The `AgentState::set_pq_seed()` method is dead code.

## Design Decision: Vault-Local BIP39

Each vault gets its own BIP39 mnemonic. This keeps vaults self-contained and avoids coupling to the global identity (which doesn't store the mnemonic on disk, so the PQ seed cannot be derived later).

The `blu identity init` command (global identity) already has PQ working. This plan makes `blu init` (vault) work the same way.

## Key Changes

The identity file (`.blu/identity.age`) needs to carry the mnemonic so the agent can derive the PQ seed on unlock. The current format only stores the X25519 secret key string. New format wraps both the key and the mnemonic in an age-encrypted blob (passphrase-protected).

The agent unlock flow will load the mnemonic from the identity file and derive the PQ seed internally, eliminating the need for a new RPC method.

## Stage 1: Identity file format with mnemonic

**Files:** `src/keys/mod.rs`, `src/keys/identity_file.rs` (new)

Create a new module for the versioned identity file format. The file stores:
- The X25519 secret key string (existing)
- The BIP39 mnemonic words (new, needed to derive PQ seed later)

When passphrase-protected, both are encrypted together. When unprotected, both are stored plaintext (same security model as today).

Add `save_identity_with_mnemonic()` and `load_identity_with_mnemonic()` functions. Keep the existing `save_identity()` / `load_identity()` for backward compat (used by `--key-file` import path).

## Stage 2: `blu init` generates BIP39 + PQ keys

**Files:** `src/cli/init.rs`, `src/config.rs`

Replace the X25519-only key generation with BIP39 flow:

1. Generate a 24-word BIP39 mnemonic
2. Derive X25519 identity via `mnemonic::derive_x25519_identity()`
3. Derive PQ recipient via `mnemonic::derive_pq_recipient()`
4. Display the mnemonic to the user with the same "write it down" warning as `blu identity init`
5. Prompt for optional mnemonic passphrase (25th word)
6. Save identity file with mnemonic using the new format from Stage 1
7. Display both public keys

Update `EncryptionConfig` to store the PQ recipient:
- Add `pq_recipient: Option<String>` field
- Serialize as `pq_recipient = "age1pq..."` in config.toml

The `--key-file` import path remains X25519-only (no mnemonic, no PQ). This is backward compatible but prints a warning that PQ is not available.

## Stage 3: Initialize KEK store with PQ recipient

**Files:** `src/cli/init.rs`

After generating keys, create the KEK store:

1. Instantiate `KekStore::new(&bludir)`
2. Call `store.init_with(&[&pq_recipient], &[pq_recipient_str])` to create the KEK wrapped for the PQ recipient
3. The KEK is also wrapped for the X25519 recipient for backward compat (both recipients in the same age file)

This creates `.blu/keys/kek.toml` and `.blu/keys/kek_v0/wrapped.age` with mlkem768x25519 stanzas.

## Stage 4: Agent unlock derives PQ seed

**Files:** `src/agent/state.rs`

Update `unlock()` and `unlock_with_secret()` to derive the PQ seed from the mnemonic stored in the identity file:

1. Load identity file with mnemonic (new format)
2. Re-derive the BIP39 seed from the mnemonic + passphrase
3. Derive PQ seed via `mnemonic::derive_pq_seed()`
4. Call `self.set_pq_seed(pq_seed)` (removes `#[allow(dead_code)]`)

The `unlock_with_secret()` path (used by biometric unlock) receives the X25519 secret key string. It needs the mnemonic to derive PQ. Options:
- The biometric CLI path can send the mnemonic separately
- Or the agent loads the mnemonic from the identity file itself

The cleaner approach: have `unlock()` load the full identity file (including mnemonic), derive both keys, and set both. The `unlock_with_secret()` path remains X25519-only unless extended.

## Stage 5: Agent protocol for PQ-aware unlock

**Files:** `src/agent/protocol.rs`, `src/agent/daemon.rs`

Add a `set_pq_seed` RPC method so the CLI can send the PQ seed after unlock. This is needed for the biometric flow where the CLI derives the seed from the device key and sends it to the agent.

Alternatively, if the agent loads the mnemonic from the identity file itself (Stage 4 approach), this RPC is not needed. The agent derives PQ internally.

Recommendation: Skip this stage. Have the agent derive PQ from the mnemonic in the identity file. This keeps the protocol simple and avoids sending additional secret material over the socket.

## Stage 6: Recovery and display commands

**Files:** `src/cli/init.rs` (new subcommand), or new `src/cli/recovery.rs`

Add a command to display the vault's mnemonic for recovery:

```
blu init show-mnemonic
```

Prompts for the identity passphrase, decrypts the identity file, and displays the 24 words.

## Stage 7: Integration tests

**Files:** `src/cli/init.rs` (tests), `src/agent/state.rs` (tests)

Add tests for:
- `blu init` generates PQ key and creates KEK store
- Agent unlock derives PQ seed and can unwrap PQ-wrapped KEK
- Full round-trip: init -> unlock -> wrap DEK -> unwrap DEK -> encrypt/decrypt
- `--key-file` import still works (X25519-only, no PQ)
- Backward compat: old identity files (no mnemonic) still load

## Execution Order

Stage 1 -> Stage 2 -> Stage 3 -> Stage 4 -> Stage 6 -> Stage 7

Stage 5 is skipped (agent derives PQ internally from the mnemonic).

## Backward Compatibility

- Old vaults (no KEK store, no PQ): continue to work with X25519-only encryption
- Old identity files (no mnemonic): agent loads X25519 key only, no PQ
- `--key-file` import: X25519-only, no PQ, no mnemonic stored
- New vaults: BIP39 + PQ by default, KEK store initialized with PQ recipient

## Files Summary

| File | Change |
|------|--------|
| `src/keys/mod.rs` | Add `identity_file` module, export new functions |
| `src/keys/identity_file.rs` | **New** - Versioned identity file with mnemonic |
| `src/config.rs` | Add `pq_recipient` field to `EncryptionConfig` |
| `src/cli/init.rs` | BIP39 flow, KEK store init, display mnemonic |
| `src/cli/clapargs.rs` | Add `show-mnemonic` subcommand (if Stage 6) |
| `src/agent/state.rs` | Derive PQ seed in `unlock()`, remove `#[allow(dead_code)]` |
| `src/agent/protocol.rs` | No changes (agent derives PQ internally) |
