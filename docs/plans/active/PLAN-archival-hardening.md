# PLAN: Archival hardening pass

## Context

Review of blu 0.8.2 (2026-10-14), looking at the repo top to bottom with
an archival lens. Findings that motivate this plan:

1. Chunking is fixed-size (`DEFAULT_CHUNK_SIZE`, 512 KiB). One byte
   inserted near the top of a file shifts every downstream boundary and
   chunk-level dedup collapses for the rest of that file. restic and
   borg both use content-defined chunking (CDC); blu's dedup is
   structurally weaker than both.
2. `blu serve` can leave the remote catalog stale: mutating handlers
   commit to redb, and the encrypted index push is debounced. A crash
   in between acknowledges writes the backend never learns about
   (already noted in TODO as the crash-atomic index-push WAL item).
3. Multi-device-safe blob GC is a hard prerequisite for orphan reclaim
   and has no design document yet.
4. Status docs drift: README says "0.7.x", START-HERE says 0.7.0 and
   was last updated 2026-07-09, ROADMAP still frames 0.5.x milestones,
   and the LOC claim (~25k) is stale against ~33.7k actual. Cargo.toml
   is 0.8.2.
5. The AWS SDK is always compiled in; feature-gating was noted as open
   in START-HERE build notes.
6. `src/serve/server.rs` is a 4.6k-LOC monolith; the next feature will
   make it worse.

## Goals

- Content-defined chunking on every write path, with identical chunk
  boundaries for identical content regardless of path (backup vs serve)
- No acknowledged `blu serve` write can be missing from the remote
  catalog after a crash and restart
- A design doc for multi-device-safe blob GC
- Docs that match reality, plus a CI guard against future drift
- A build without the AWS SDK for local-only users
- `serve/server.rs` split into reviewable modules with no behavior
  change

## Non-goals

- Changing the encryption pipeline, envelope format, or index format
- Implementing multi-device GC (design document only)
- redb at-rest encryption, delta sync, or WAL work beyond the serve
  index push
- v2 vs v3 latency benchmarks
- Splitting `blob.rs`, `config.rs`, or `redb_store.rs` (revisit when
  they grow)
- KEK rotation and multi-user access (separate roadmap items)

## Design notes

### Content-defined chunking

- File identity is already chunking-independent: `hash_and_add_file`
  hashes file content with a streaming hash across chunks
  (`src/block/index.rs:151-159`). Changing chunk boundaries does not
  change file hashes, catalog identity, tombstones, or multi-device
  merge behavior. Existing vaults remain valid; only new and changed
  files chunk differently.
- One shared profile, defined once: min 128 KiB, avg 512 KiB, max
  4 MiB. The 512 KiB average preserves the current operational scale
  (chunk counts, blob packing, the EBS-granularity rationale) while
  gaining shift resilience. Tuning happens in one const.
- Use `fastcdc` 5.x (MIT, pure Rust): the streaming reader API for the
  file path, the slice API for in-memory data. Confirm exact module
  paths against docs.rs at implementation time.
- Three write paths must share the implementation:
  1. `Chunkerator` (`src/block/chunkerator.rs`), used by
     `hash_and_add_file` for backup/status walks
  2. The fixed-size read loop in `put_object_inner_from_file`
     (`src/serve/server.rs:1147`), which currently allocates
     `vec![0u8; DEFAULT_CHUNK_SIZE]`
  3. `chunk_bytes`, the in-memory counterpart
- `DEFAULT_BLOB_CAPACITY_BYTES` (`src/blob.rs:49`) currently derives
  from `DEFAULT_CHUNK_SIZE << 7` (64 MiB). Pin it explicitly at 64 MiB
  so blob packing does not silently change with the profile.
- Tests: replace exact-boundary fixture assertions (`test/blocks/t1`)
  with property tests: round-trip reassembly, min/max bounds,
  prepend-a-byte chunk reuse, and identical boundaries from the file
  and in-memory APIs. Keep file-hash assertions; they must not move.
- CHANGELOG entry under Unreleased / Changed.

### Crash-atomic index push (serve WAL)

Current flow: mutating handlers commit redb while holding
`write_mutex`; `flush_indexes` (`src/serve/server.rs:1294`) dumps redb,
writes encrypted indexes, and calls `push_indexes`, debounced by
`schedule_flush`. A crash between the redb commit and the push leaves
the remote catalog stale. Restart re-populates redb from the backend
(`src/serve/index_sync.rs`), so the window must be closed before that
can mask data.

- Dirty marker: `.blu/serve.dirty`, written and fsynced while holding
  `write_mutex` before any mutating redb commit; removed (with the
  parent directory fsynced) only after a successful `push_indexes`,
  still under `write_mutex`.
- Startup: if the marker exists, run `flush_indexes` before the daemon
  reports ready (the health endpoint already has a not-ready state).
  Retry with backoff; stay not-ready while the flush fails.
- Idempotence: a spurious marker (crash after push, before unlink)
  causes one extra flush and is harmless.
- Audit every mutating path (put, delete, multipart complete, multipart
  abort, any tag mutation) to confirm marker discipline.
- Tests: crash injection (commit redb, drop state before flush,
  restart) proves the remote catalog contains the write; marker
  lifecycle tests; failed push keeps the marker.

### Multi-device GC design

`docs/design/MULTI_DEVICE_GC_DESIGN.md` covering:

- Tombstones and LWW interaction with blob reachability
- Grace periods vs refcounts vs generation-based reachability, with a
  recommendation
- Preconditions for orphan reclaim (`blu doctor` warn, then an explicit
  operator destroy command; never auto-delete)
- The safety argument for peers that are offline during a reclaim

### Docs refresh and drift guard

- README: status line to the current release line, config example
  `blu_version`, feature list accuracy pass
- START-HERE.md: rewrite for 0.8.2 (LOC, test count, shipped: progress
  UI, S3 backpressure, cold storage and thaw, headless automation;
  remaining gaps)
- ROADMAP.md: reframe the pre-release line, mark 0.5.0 as historical
- `scripts/check-docs.sh`: assert version markers in README and
  START-HERE match `Cargo.toml`; wire into CI

### S3 feature gating

- `[features] default = ["s3"]`; `aws-config` and `aws-sdk-s3` become
  optional dependencies
- Gate `src/storage/s3.rs`, `BackendKind::AmazonS3` and its match arms
  (`src/storage.rs:119-213`), S3 config parsing (clear error when
  `type = "s3"` without the feature), and CLI paths that need S3
  (`open --type s3`, `backend add --type s3`, `thaw`, doctor cold
  checks, `intelligent-tiering print`)
- CI: add an ubuntu job that builds with `--no-default-features` (and
  runs tests where feasible)

### serve/server.rs module split

Pure moves, behavior identical, tests stay inline per repo convention:

- `src/serve/mod.rs`: state, bootstrap, shutdown, router wiring
- `src/serve/handlers/object.rs`: get/head/put/delete/list handlers
- `src/serve/handlers/multipart.rs`: multipart handlers
- `src/serve/handlers/bucket.rs`: bucket-level handlers
- `src/serve/http_util.rs`: range parsing, HTTP dates, body helpers,
  error mapping
- `src/serve/flush.rs`: `flush_indexes`, `schedule_flush`, cold
  preflight helpers

## Stages

Stage 1: Content-defined chunking
  1a: Add the fastcdc dependency; define the chunk profile const and
      pin blob capacity at 64 MiB (`src/block.rs`, `src/blob.rs`)
  1b: Replace `Chunkerator` internals with streaming CDC, preserving
      the `Iterator<Item = Vec<u8>>` API
  1c: Replace `chunk_bytes` with slice-based CDC using the same profile
  1d: Switch the serve upload loop (`src/serve/server.rs:1147`) to the
      shared CDC path
  1e: Rework chunk fixture tests into property tests (reassembly,
      bounds, shift resilience, cross-path identity); keep file-hash
      assertions
  1f: CHANGELOG entry; cargo test, clippy, fmt green

Stage 2: Crash-atomic index push
  2a: Dirty-marker write and clear in the flush path under
      `write_mutex`
  2b: Startup flush-if-dirty before the daemon reports ready
  2c: Audit all mutating handlers for marker discipline
  2d: Crash-injection and marker-lifecycle tests

Stage 3: Multi-device GC design doc
  3a: Write `docs/design/MULTI_DEVICE_GC_DESIGN.md`
  3b: Point TODO.md at the design

Stage 4: Docs refresh and drift guard
  4a: README refresh (status, version references, features)
  4b: START-HERE rewrite for the current release
  4c: ROADMAP refresh
  4d: `scripts/check-docs.sh` plus CI wiring

Stage 5: S3 feature gating
  5a: Cargo features and optional AWS dependencies
  5b: cfg-gate storage, config parsing, and CLI paths
  5c: CI `--no-default-features` job
  5d: Verify a slim build and a local-only vault flow end to end

Stage 6: serve/server.rs module split
  6a: Extract `http_util`
  6b: Extract handlers and flush modules
  6c: Rewire `mod.rs`; no behavior change
  6d: Full test suite and serve smoke green

## Acceptance

- Prepending one byte to an indexed file yields at least 90 percent
  chunk reuse on re-index (today: near zero)
- Identical content chunked via backup and via `blu serve` produces
  identical chunk lists
- kill -9 after a serve PUT is acknowledged, then restart: the remote
  catalog contains the object and `blu doctor` is clean
- README, START-HERE, and ROADMAP reflect the current release; the CI
  drift check fails on stale version markers
- `cargo build --no-default-features` succeeds and local-only vault
  flows pass tests
- No `src/serve/*.rs` file above roughly 1.5k LOC after the split; full
  suite, clippy, and fmt green

## References

- `src/block.rs`, `src/block/chunkerator.rs`, `src/block/index.rs`
- `src/blob.rs` (`DEFAULT_BLOB_CAPACITY_BYTES`)
- `src/serve/server.rs` (`flush_indexes`, `put_object_inner_from_file`),
  `src/serve/index_sync.rs`, `src/serve/redb_store.rs`
- `docs/project/START-HERE.md`, `docs/project/ROADMAP.md`,
  `docs/project/TODO.md`
- `docs/plans/archived/PLAN-blake3-content-hash.md` (prior content
  identity change)
- fastcdc crate (nlfiedler/fastcdc-rs), MIT
