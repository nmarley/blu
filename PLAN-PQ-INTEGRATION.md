# PQ Integration Plan

Finishes the post-quantum integration for `blu init`. The crypto
primitives are complete and tested. This plan wires them into the
vault initialization and agent unlock flows.

Written April 2026 after reviewing PLAN-PQ.md, ENVELOPE_ENCRYPTION_DESIGN.md,
and the existing implementation.

## Problem

`blu init` only generates X25519 keys. The PQ stack (hybrid KEM,
HPKE, age PQ recipient/identity, BIP39 derivation, KEK store) is
fully implemented and tested at the library level, but is never
invoked from the vault init command. `AgentState::set_pq_seed()` and
`AgentState::has_pq()` are dead code.

## Design: Global Identity, Per-Vault KEK

Identity is global (per user, in `~/.blu/`), not per vault. This is
consistent with every design document in the repo:

- PLAN-PQ.md: one BIP39 mnemonic derives all key types
- ENVELOPE_ENCRYPTION_DESIGN.md line 124: "UK is vault-independent.
  The same mnemonic = same UK = same identity across all vaults.
  This is intentional - your identity follows you."
- PLAN.md line 561: "Identity is global (per user). Vault init is
  separate."
- identity_cmd.rs line 4: "Identity is global (per user, lives in
  ~/.blu/), not per-vault."

`blu identity init` already generates both X25519 and PQ keys from
the mnemonic and stores both public keys in `~/.blu/identity.toml`.
`blu init` needs to read those public keys and use them to wrap the
vault's KEK. No new mnemonic generation. No new identity file format.

The PQ seed is derived at runtime, never stored on disk:

- Biometric path: `biometric::unlock()` recovers the 64-byte BIP39
  Seed from `~/.blu/identity.enc`. The PQ seed is derived from that
  Seed via `mnemonic::derive_pq_seed()`.
- Passphrase path: the mnemonic is not available (intentionally not
  stored on disk). The agent cannot derive PQ internally from
  `identity_path` + `passphrase` alone. The CLI must derive it and
  send it via RPC. See Stage 3.

## What Already Works

Before listing changes, here is what is already wired up and needs
no modification:

- `AgentState::load_kek()` (state.rs:310) already builds a list of
  identities with PQ first, X25519 fallback. Once `set_pq_seed()` is
  called, PQ-wrapped KEKs decrypt automatically.
- `handle_wrap_dek` and `handle_unwrap_dek` in daemon.rs lazily call
  `state.load_kek(kek_dir)` on first use. No changes needed.
- `KekStore::init_with()` accepts `&[&dyn age::Recipient]` and works
  with PQ recipients (tested in kek.rs `pq_store_init_and_unwrap`).
- All PQ crypto primitives: hybrid_kem, hpke, pq, mnemonic derivation.

## Stage 1: `EncryptionConfig` gains PQ recipient field

**Files:** `src/config.rs`

Add an optional PQ recipient field to `EncryptionConfig`:

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pq_recipient: Option<String>,

The field is `Option` for backward compatibility: old config.toml
files without this field deserialize cleanly with `None`.

New vaults get both `recipient` (X25519) and `pq_recipient`
(mlkem768x25519) written to config.toml.

## Stage 2: `blu init` reads global identity, creates KEK store

**Files:** `src/cli/init.rs`

Replace the X25519-only key generation with a flow that reads the
global identity:

1. Check for `~/.blu/identity.toml`
  1a. If found, read both `public_key` and `pq_public_key` from it
  1b. If not found and `--key-file` was provided, import X25519 key
      (existing behavior), print warning that PQ is not available,
      skip KEK store creation
  1c. If neither found, error with message: "no global identity found
      (run `blu identity init` first)" and suggest `--key-file` for
      legacy X25519-only mode

2. Read the X25519 identity from `~/.blu/identity.age` (or
   `--key-file` if provided)

3. Save the identity to `.blu/identity.age` (existing behavior,
   keeps vault self-contained for the passphrase unlock path)

4. Write config.toml with both `recipient` and `pq_recipient`

5. Initialize the KEK store:
  5a. Parse the PQ recipient string into a `PqRecipient`
  5b. Parse the X25519 recipient string into an `x25519::Recipient`
  5c. Call `KekStore::init_with()` with both recipients, so the
      wrapped.age file contains both mlkem768x25519 and X25519
      stanzas (either identity can unwrap it)
  5d. This creates `.blu/keys/kek.toml` and
      `.blu/keys/kek_v0/wrapped.age`

6. Create indexes and empty index file (existing behavior)

When `--key-file` is used (legacy import), no KEK store is created
and no PQ recipient is stored. The vault operates in X25519-only
mode, same as today.

## Stage 3: `unlock_with_secret` RPC gains optional PQ seed

**Files:** `src/agent/protocol.rs`, `src/agent/daemon.rs`,
`src/agent/state.rs`, `src/agent/client.rs`,
`src/cli/agent_cmd.rs`

The biometric path already has the full BIP39 Seed (recovered from
`~/.blu/identity.enc` via Touch ID). Today it derives only the X25519
identity and sends the secret key string to the agent. It should also
derive the PQ seed and send it.

3a. Extend `unlock_with_secret` RPC to accept an optional `pq_seed`
    parameter (base64-encoded 32 bytes):

    In daemon.rs `handle_unlock_with_secret`:
      - After calling `state.unlock_with_secret(secret)`, check for
        `params["pq_seed"]`
      - If present, base64-decode to 32 bytes, construct
        `HybridSeed::new()`, call `state.set_pq_seed()`

    In client.rs:
      - Add `unlock_with_secret_pq(secret, pq_seed_bytes)` method
        that sends both fields

3b. Update `try_biometric_unlock` in agent_cmd.rs:
      - After deriving X25519 identity from seed, also derive PQ seed
        via `mnemonic::derive_pq_seed(&seed)`
      - Call `client.unlock_with_secret_pq(secret_str, pq_seed_bytes)`

3c. The passphrase path (`handle_unlock` in daemon.rs) does not have
    the mnemonic or BIP39 seed. It cannot derive PQ. For now, the
    passphrase unlock path is X25519-only for KEK unwrapping.
    `load_kek()` will fall back to the X25519 identity, which works
    because Stage 2 wraps the KEK to both recipients.

    This is acceptable because:
    - The KEK wrapped.age contains both stanza types (Stage 2, step 5c)
    - X25519 identity can always unwrap KEKs created by our init
    - The PQ stanza provides harvest-now-decrypt-later protection for
      the KEK blob at rest; the X25519 stanza is the online fallback
    - Future work: if the passphrase path needs PQ-only KEK unwrap
      (e.g., after X25519 stanzas are removed in a future KEK
      rotation), the CLI can prompt for the mnemonic and derive PQ

3d. Remove `#[allow(dead_code)]` from `set_pq_seed()` and `has_pq()`
    in state.rs.

## Stage 4: Integration tests

**Files:** tests in `src/cli/init.rs`, `src/agent/state.rs`

Tests to add:

- `blu init` with global identity present: creates KEK store,
  config.toml has both recipients, wrapped.age is decryptable by
  both PQ and X25519 identities
- `blu init --key-file`: no KEK store, no PQ recipient in config,
  prints warning
- `blu init` without global identity and without `--key-file`: errors
  with helpful message
- Agent unlock via biometric path sets PQ seed, `load_kek()` uses PQ
  identity to unwrap KEK
- Agent unlock via passphrase path: no PQ seed, `load_kek()` falls
  back to X25519 identity
- Full round-trip: init -> unlock -> wrap DEK -> unwrap DEK
- Backward compat: old vaults without KEK store still work
- Backward compat: old config.toml without `pq_recipient` loads fine


## Execution Order

Stage 1 -> Stage 2 -> Stage 3 -> Stage 4

Each stage is one commit.


## Backward Compatibility

- Old vaults (no KEK store, no PQ): continue to work unchanged with
  X25519-only encryption via BlackBox
- Old config.toml (no `pq_recipient` field): deserializes as `None`,
  no behavior change
- `--key-file` import: X25519-only, no KEK store, no PQ
- New vaults: KEK store initialized with both PQ and X25519
  recipients, PQ recipient in config.toml


## Files Summary

| File | Change |
|------|--------|
| `src/config.rs` | Add `pq_recipient: Option<String>` to `EncryptionConfig` |
| `src/cli/init.rs` | Read global identity, create KEK store with both recipients |
| `src/agent/state.rs` | Remove `#[allow(dead_code)]` from PQ methods |
| `src/agent/daemon.rs` | Extend `handle_unlock_with_secret` to accept optional `pq_seed` |
| `src/agent/client.rs` | Add `unlock_with_secret_pq()` method |
| `src/agent/protocol.rs` | No changes (reuses `UnlockWithSecret` method) |
| `src/cli/agent_cmd.rs` | Derive PQ seed in `try_biometric_unlock`, send to agent |


## Out of Scope

- Multi-user access (PLAN-PQ.md Stage 3): separate plan
- Recovery kit CLI (PLAN-PQ.md Stage 4): separate plan
- Removing X25519 stanzas from KEK wraps: future KEK rotation concern
- PQ for the BlackBox encrypt/decrypt path (file-level encryption):
  the threat model in PLAN-PQ.md identifies the UK->KEK asymmetric
  layer as the vulnerability; the symmetric layers (KEK->DEK, DEK->data)
  are already quantum-safe (256-bit keys)
