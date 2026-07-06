# Alpha Readiness Analysis

Date: 2026-05-18

## Summary

End-to-end smoke testing of the existing `data/v2/` vault and attempted
fresh vault creation surfaced critical issues that block alpha use.

## Build Health

Compilation, clippy, and formatting are clean. 206 tests pass, 0 fail,
2 ignored (slow scrypt tests). The foundation is solid.

## Critical: Bincode Backward Compatibility (all indexes)

`BlobIndex` deserialization fails with "unexpected end of file" on any
index written before commit `76897d9` (2026-05-15), which added the
`paths_to_repack` field.

Root cause: all three index types (`PlainIndex`, `BlobIndex`, `TagIndex`)
use `bincode` for serialization via the `gen_std_enc_serde!` macro in
`src/io.rs`. Bincode is a **non-self-describing** binary format. It reads
fields sequentially by position and has no concept of field names,
optional fields, or defaults. The `#[serde(default)]` attribute on
`paths_to_repack` has zero effect with bincode; it only works with
self-describing formats (JSON, TOML, CBOR, MessagePack, etc.).

This means any struct change (adding a field, removing a field, reordering
fields) silently breaks deserialization of every previously-written index.
The same landmine exists for `PlainIndex` and `TagIndex`; they just
have not been hit yet because their schemas have not changed recently.

Impact: `restore-files`, `delete-files`, `defrag-blobs`, `encrypt-files`,
and `status` (blob summary) all fail on the existing vault. This is the
single biggest alpha blocker.

Fix: replace bincode with a self-describing format for all index
serialization. `serde_cbor` is already a dependency. CBOR is compact,
self-describing, and `#[serde(default)]` works correctly. No external
users exist, so there is no migration burden; old indexes simply need
to be re-created (re-sync).

Files affected:
- `src/io.rs` (the `gen_std_enc_serde!` macro, lines 44-46 and 49-51)
- `Cargo.toml` (bincode dependency can be removed after migration)
- All existing `.blu/indexes/*.dat` files become unreadable (expected;
  re-sync to regenerate)

## Bug: `blu init` Cannot Use the Unlocked Agent

`init` reads the PQ seed directly from `~/.blu/identity.age` via
`load_global_identity_pq_seed()` at `src/cli/init.rs:119`. If the
identity file is passphrase-encrypted, it prompts via `rpassword`,
which requires a TTY. There is no code path to delegate to the already-
unlocked agent daemon.

The `--no-passphrase` flag means "the key file is unencrypted," not
"use the agent." This is confusing naming.

Impact: cannot init vaults from scripts or non-interactive contexts.
Minor for alpha (TTY is available), but a real UX gap.

## Observation: Config Drift

The `data/v2/.blu/config.toml` contains `prune_deleted` and
`prune_dangling` fields not present in the current `Config` struct.
These are silently ignored by TOML deserialization (serde skips unknown
fields by default), so they cause no runtime errors. They are leftovers
from an older version of the config format.

## Observation: Tag Index Not Created by Init

`blu init` creates an empty `index.dat` (plain index) but does not
create `tag_index.dat` or `blob_index.dat`. These are created lazily
on first use. This means `status` and other commands that try to load
all three indexes must handle "file not found" gracefully. The status
command does handle blob index absence, but prints an error message
that looks like a bug to the user rather than a normal "no blobs yet"
state.

## Priority Order

1. Replace bincode with CBOR for all index serialization (critical)
2. Verify with full end-to-end smoke test on fresh vault
3. Fix init agent integration (nice-to-have for alpha)
4. Clean up status output for missing indexes (polish)
