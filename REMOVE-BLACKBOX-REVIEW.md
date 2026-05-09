# Remove BlackBox Review

## Verdict

The analysis in `REMOVE-BLACKBOX.md` is directionally correct. The core
claim is true: after the v1 removal, `BlackBox` no longer represents a
general encryption black box. It is now mostly a dispatcher between local
and agent-backed DEK wrapping, while bulk payload encryption is already
performed locally with a DEK.

I agree with removing or replacing `BlackBox`, but the proposed
`DekProvider` interface is too thin as written. A production-quality
replacement needs to model KEK loading, KEK version resolution, agent cache
behavior, and ownership needs across the existing callsites.

## What the analysis gets right

1. `BlackBox` is now at the wrong abstraction level.

   The current v2 data path separates bulk encryption from key wrapping.
   Payload bytes are encrypted locally with `Dek::encrypt_data()`, while the
   agent path only supplies a plaintext DEK plus a wrapped DEK.

2. The agent path follows the KMS pattern.

   Agent-backed encryption currently does this:

   1. Ask the agent to generate and wrap a DEK.
   2. Convert the returned DEK bytes into a local `Dek`.
   3. Encrypt payload data locally.
   4. Assemble the v2 envelope locally.

   That is key management service behavior, not general-purpose data
   encryption behavior.

3. The in-process `identities` field is dead weight for data operations.

   `BlackBoxInner::InProcess { identities }` still stores X25519 age
   identities, but v2 encrypt and decrypt paths use the attached `KekContext`
   instead. The identities are not used for blob or index encryption.

4. The name `BlackBox` is misleading.

   The name suggests a broad encryption/decryption engine. In practice, the
   meaningful boundary is now DEK wrapping and unwrapping. A key-provider or
   envelope-key abstraction would communicate intent better.

5. Passphrase encryption is a separate concern.

   `passphrase_encrypt` and `passphrase_decrypt` protect identity files.
   They should not be coupled to the data envelope path.

## Gaps and corrections

1. The proposed `DekProvider` lacks KEK loading context.

   The suggested trait has `wrap_dek(&self)` and
   `unwrap_dek(&self, wrapped, version)`, but the agent currently may need a
   vault `.blu` path to load the KEK on demand. Without that, agent-backed
   encryption can fail unless the KEK has already been preloaded out of band.

2. The proposed local provider only models one KEK.

   `LocalKeyProvider { kek, kek_version }` is enough for the current active
   KEK, but not for historical KEK versions. Since v2 headers store a KEK
   version, the replacement should resolve the requested version from a KEK
   store or equivalent cache.

3. The current code already has KEK version limitations.

   In-process decryption currently ignores the requested version and uses the
   cloned current KEK. Agent decryption rejects any requested version other
   than its cached version. A refactor should not preserve that limitation
   unless version rotation is explicitly out of scope.

4. Agent state uses `blackbox` as an unlocked-state sentinel.

   The doc says the agent can replace `blackbox: Option<BlackBox>` with the
   KEK cache it already has. That is mostly true for key operations, but the
   current `is_unlocked()` implementation checks whether `blackbox` is set.
   Removing it requires an explicit replacement for unlocked state, such as
   checking the decrypted secret key or adding a dedicated session state.

5. The deletion section contradicts itself.

   The doc says to delete all of `src/age.rs`, but also says to keep
   `passphrase_encrypt` and `passphrase_decrypt`, which currently live there.
   Either `age.rs` should remain with only passphrase helpers, or those
   helpers should move to a clearer module such as `keys/identity_crypto.rs`.

6. The migration scope is incomplete.

   The listed touchpoints are valid, but the replacement also needs to flow
   through the CLI helper path and commands that call `load_config_and_blackbox()`.
   That includes encrypt, restore, sync, status, search, list, delete, and
   defrag paths.

7. The trait shape may cause ownership churn.

   `BlobBuffer` currently owns a cloneable `BlackBox`, while `EncBlobReader`
   borrows one. A borrowed `&dyn DekProvider` may work in some places, but an
   owned provider enum or `Arc<dyn DekProvider + Send + Sync>` may reduce
   lifetime friction.

## Recommended replacement shape

The right replacement is still a key-management abstraction, but it should be
slightly richer than the draft in `REMOVE-BLACKBOX.md`.

```rust
trait DekProvider {
    fn wrap_dek(&self) -> Result<(Dek, Vec<u8>, u16)>;
    fn unwrap_dek(&self, wrapped: &[u8], version: u16) -> Result<Dek>;
}
```

That trait is the right conceptual center, but the concrete providers should
own enough context to make those methods reliable.

1. `LocalDekProvider`

   Should be backed by a KEK resolver, not just one `Kek`. It can cache the
   active KEK for writes, but reads should resolve by version.

2. `AgentDekProvider`

   Should know the vault `.blu` path or otherwise ensure that the agent can
   load the correct KEK before wrapping or unwrapping. It should hide the
   current `kek_dir` preload requirement from callers.

3. Envelope functions

   Data encryption and file assembly can become free functions or methods on
   a thin vault object that accepts a `DekProvider`.

4. Identity passphrase helpers

   These should move out of the data encryption abstraction, or remain in a
   narrowly named module that does not imply blob or index encryption.

## Suggested staged plan

Stage 1: Introduce the replacement abstraction
  1a: Add a `DekProvider` trait in a key-management module.
  1b: Add envelope helper functions for blob and index encrypt/decrypt.
  1c: Keep `BlackBox` as a compatibility wrapper temporarily.

Stage 2: Implement local and agent providers
  2a: Implement a local provider with active KEK support for writes.
  2b: Add version-aware KEK resolution for reads, or clearly document it as
      a follow-up if rotation is not implemented yet.
  2c: Implement an agent provider that carries vault context and handles KEK
      loading internally.

Stage 3: Migrate callsites
  3a: Update index serialization and config read/write helpers.
  3b: Update blob write and blob read paths.
  3c: Update CLI helper loading and every command that receives the current
      `BlackBox`.

Stage 4: Simplify agent state
  4a: Replace `blackbox: Option<BlackBox>` with explicit unlocked state.
  4b: Keep secret key, PQ seed, and KEK cache responsibilities separate.
  4c: Preserve timeout, zeroization, and memory-locking behavior.

Stage 5: Delete the compatibility layer
  5a: Remove `BlackBox`, `BlackBoxInner`, and `KekContext`.
  5b: Move or narrow `src/age.rs` so passphrase helpers are not tied to the
      old data-path abstraction.
  5c: Remove `keys::blackbox_from_identity()` and update tests.

## PQ-encryption note

The pasted `wrapped.age` header appears to be post-quantum hybrid encrypted.
The relevant stanza is:

```text
-> mlkem768x25519 ...
```

That is the age recipient type for ML-KEM-768 plus X25519 hybrid encryption.
It is not pure PQ encryption; it is hybrid PQ plus classical X25519. That is
the desirable construction for age's current post-quantum recipient type.

The pasted header also includes a GREASE stanza. That does not indicate a
classical recipient. I did not see an `-> X25519` stanza in the pasted header,
so based on the visible data, the KEK wrapper appears to be PQ-hybrid-only.

## Bottom line

Removing `BlackBox` is the right direction. The objective correction is that
the replacement should be a full envelope key provider with vault context and
version-aware KEK resolution, not only a minimal DEK wrapping trait. If the
refactor handles those concerns, it should reduce technical debt rather than
moving the current awkwardness into a new name.
