# Start Here

Last updated: 2026-07-09

Version: **0.5.0** (pre-release dogfood / late-alpha). Crate version and
docs use 0.5.0; there is no separate public v0.1.0-alpha tag story.

## Overall Assessment

The cryptographic core (envelope encryption, PQ hybrid KEK wrapping,
ChaCha20-Poly1305 pipeline, agent daemon with mlock'd memory) is solid
and well-tested. The content-addressed storage model, named multi-backend
config system, and backend mirror/diff commands are polished. The data
pipeline (sync, delete cascade, defrag, restore) works end-to-end.

Recent dogfood-readiness work: `.bluignore`, `blu doctor`, vault pipeline
smoke tests, GitHub Actions CI (`macos-15` + `ubuntu-24.04`), README and
changelog rewrite. Index serde is ciborium (CBOR). New blobs are v3
segmented AEAD. Scrypt work factor for identity files is pinned to N ≥ 18.

~25k LOC under `src/`, ~350+ unit tests (plus smokes). Clippy clean.

## Area-by-Area Status

### Encryption Pipeline: SOLID

Envelope encryption, ChaCha20-Poly1305 bulk encryption, PQ hybrid KEK
wrapping (ML-KEM-768 + X25519), agent daemon with zeroize-on-drop. All
well-tested with tests concentrated in keys/, agent/, and format modules.

### Storage Backends: SOLID

Named multi-backend config with clean serde tagging and legacy migration.
`BackendKind` enum dispatch (`Local`, `AmazonS3`). Indexes are local-first,
pushed/pulled via sync. `backend mirror` and `backend diff` are polished.

Missing: no `list` method on backends (orphan blob discovery blocked);
no S3 field validation in config.

### v3 Segmented AEAD Blob Format: SHIPPED

v3 replaces v2's single sealed AEAD box with fixed-size, independently
authenticated segments (counter-derived nonces; header fields in AAD).
Enables prefix-fetch reads. Indexes (`BLUI`) remain v2. Upgrade path:
`defrag-blobs --upgrade-format`.

### Status / Delete / Defrag: COMPLETE

`status`, `delete-files` (full cascade + `--scrub`), and `defrag-blobs`
are production-quality with shared repack logic.

### Search / Tags: WORKING (partial)

Basic substring search; tag add/remove/list. Search index not persisted.

### `.bluignore`: SHIPPED

gitignore-style via the `ignore` crate. Shared walker for add/sync/status.
Always excludes `.blu/` and `.git/`. Explicit single-file paths override.

### `blu doctor`: SHIPPED (structural checks)

Config, encryption, KEK store, agent warn, index decrypt, version,
cross-refs, encryption coverage, GC queues, `backend.exists` for indexed
blobs. No orphan scan (needs backend `list`). No `--repair`.

### `blu serve`: WORKING

S3-compatible local daemon: Get/Head/List/Put/multipart/Delete, redb
index, debounced flush, graceful shutdown, traffic countermeasures,
large-body streaming. Hardening pass complete.

### Test Coverage

Strong crypto/format/serve coverage plus e2e smokes in `src/cli/smoke.rs`.
Per-command CLI entry-point suites still thin (Tier 2 backlog).

### Error Handling: GOOD

`BluError` via thiserror. Bare unwraps removed from production CLI.

## Build Notes

- S3 deps always compiled in; feature-gating still open
- `security-framework` is macOS-only target dependency (Linux builds clean)
- CI present; changelog present; no release publish workflow yet

## Non-goals for dogfood

Full multi-user, KEK rotation CLI, recovery kit PDF, backend orphan GC,
redb at-rest encryption, crash-atomic index WAL, v2/v3 latency benchmarks.
