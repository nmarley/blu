# Async Backend Refactor Spec

## Problem

`src/bin/blu.rs` uses `#[tokio::main]`, which starts a Tokio runtime.
`src/storage/s3.rs` creates its own `tokio::runtime::Runtime` inside
`AmazonS3::new()` and calls `block_on()` in every trait method. When
any CLI command initializes an S3 backend, Tokio panics with "Cannot
start a runtime from within a runtime."

## Root cause

Two competing runtime strategies that worked in isolation but clash
when combined:

- Outer: `#[tokio::main]` in the entrypoint
- Inner: private `Arc<Runtime>` in `AmazonS3`, with `block_on()` in
  every method

## Design decisions

### Enum dispatch (not dyn trait)

Native `async fn` in traits (stable since Rust 1.75) is not
object-safe, so `Box<dyn Backend>` and `&dyn Backend` cannot be used
with async methods. Since there are exactly two backend variants
(`Local` and `AmazonS3`), we replace dynamic dispatch with a concrete
`BackendKind` enum. This is zero-cost, requires no extra crates, and
is the canonical Rust approach for a closed set of variants.

The alternative (`async-trait` crate to box futures) is a legacy shim
and adds an unnecessary dependency.

### tokio::fs for Local backend

The `Local` backend currently uses `std::fs` (blocking I/O). Wrapping
blocking calls in `async fn` would block the Tokio runtime thread.
This refactor converts `Local` to use `tokio::fs` for proper
non-blocking file I/O.

## Affected files

### Trait and implementations

- `src/storage.rs` -- trait definition, enum dispatch, re-exports
- `src/storage/local.rs` -- `Local` impl (sync fs to tokio::fs)
- `src/storage/s3.rs` -- `AmazonS3` impl (remove private Runtime)

### Config layer

- `src/config.rs` -- factory methods (`build_backend`,
  `init_named_backend`, `init_storage_backend`), index push/pull
  methods

### Blob layer

- `src/blob.rs` -- `BlobBuffer` and `EncBlobReader` (hold backend
  references, call backend methods)

### CLI consumers

- `src/cli/backend_cmd.rs` -- `mirror`, `diff`, `list --stats`
- `src/cli/encrypt_files.rs` -- `BlobBuffer::new`
- `src/cli/restore_files.rs` -- `EncBlobReader::new`
- `src/cli/sync.rs` -- `BlobBuffer::new` + `push_indexes`
- `src/cli/pull.rs` -- `pull_indexes`

### Entrypoint

- `src/bin/blu.rs` -- `run()` becomes async

## Stages

### Stage 1: Enum dispatch (sync, mechanical refactor)

Pure type-level refactor. All behavior identical. Code compiles and
tests pass after this stage.

1a. Define `pub enum BackendKind { Local(Local), AmazonS3(AmazonS3) }`
    in `src/storage.rs`.

1b. Implement the 6 methods on `BackendKind`, delegating to the inner
    `Backend` trait impls via match arms.

1c. Replace `Box<dyn Backend>` returns with `BackendKind` in
    `Config::build_backend`, `init_named_backend`,
    `init_storage_backend`.

1d. Replace `&dyn Backend` params with `&BackendKind` in
    `Config::push_indexes`, `pull_indexes`,
    `write_index_to_backend`.

1e. Change `BlobBuffer.storage_backend` from
    `&'a (dyn Backend + 'a)` to `&'a BackendKind`.

1f. Change `EncBlobReader.backend` from
    `&'b (dyn Backend + 'b)` to `&'b BackendKind`.

1g. Update CLI consumers: remove `&*backend` / `&(*backend)` deref
    dance (no longer needed with a concrete type).

1h. Remove the `Backend` trait (dead code after enum dispatch).

### Stage 2: Convert to async

This stage eliminates the nested runtime panic.

2a. Make all 6 `BackendKind` methods `async fn`.

2b. `AmazonS3`: remove `Arc<Runtime>` field. Make `new()` async.
    Rewrite all methods as plain async (`.await` on AWS SDK calls,
    no `block_on`).

2c. `Local`: convert from `std::fs` to `tokio::fs`. All methods
    become genuinely async.

2d. Make `Config` methods async: `build_backend`,
    `init_named_backend`, `init_storage_backend`, `push_indexes`,
    `pull_indexes`, `write_index_to_backend`.

2e. Make `BlobBuffer` methods async: `write_chunk`, `finalize`,
    `write_blob`, `roll_new_blob`.

2f. Make `EncBlobReader::get_bytes` async.

2g. Make CLI handler functions async: `backend` dispatcher,
    `mirror`, `list`, `diff`, `encrypt_files`, `restore_files`,
    `sync`, `pull`.

2h. Make `run()` in `src/bin/blu.rs` an `async fn`; `main` calls
    `run().await`.

2i. Remove `use tokio::runtime::Runtime` and `use std::sync::Arc`
    from `src/storage/s3.rs`.

### Stage 3: Verify and clean up

3a. `cargo fmt -- --check`
3b. `cargo clippy`
3c. `cargo test`
3d. `cargo test -- --ignored`
3e. Fix any dead imports or unused code flagged by clippy.
3f. Manual smoke test: `blu backend mirror --from default --to bcde`

## Out of scope

- Streaming reads/writes (existing TODO in Backend trait)
- Concurrent blob transfers in mirror (future enhancement)
- Additional backend types (GCS, Azure, DO)
- async-trait crate (legacy, not needed)
