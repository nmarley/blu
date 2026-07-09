# blu dogfood closeout

Residual work after `PLAN-dogfood-readiness.md` (stages 1-7 landed).
Scope: green CI, retire the old plan, then manual dogfood.

Progress is recorded by git history and the working conversation, not
by mutating this file.

## Stage 1: Make Linux clippy clean

1a. Gate or drop unused `Dek` import on non-macOS in
    `src/agent/biometric.rs`.
1b. Doc comments on non-macOS stubs (`is_available`, `setup`,
    `unlock`, `remove`).
1c. Drop redundant `&` in `src/cli/encrypt_files.rs` and
    `src/hash.rs` format args (`clippy::useless_borrows_in_formatting`).
1d. `cargo clippy -- -D warnings` clean locally.

## Stage 2: Deflake agent idle-timeout test

2a. Rewrite `touch_resets_idle_timer` (and sibling sleep-based
    timeout tests if needed) so they are not wall-clock fragile.
2b. Confirm the test is stable under parallel `cargo test`.

## Stage 3: Retire old plan

3a. Delete root `PLAN-dogfood-readiness.md` (git history is the
    archive; same pattern as prior completed plans).

## Stage 4: Manual dogfood + green CI (human)

4a. Push and confirm GHA green on `macos-15` + `ubuntu-24.04`.
4b. Fresh vault path: `identity init` -> `unlock` -> `init` ->
    files + `.bluignore` -> `sync` -> `status` -> `doctor` ->
    `serve` smoke -> `restore-files` -> `delete-files` ->
    `defrag-blobs`.
4c. Stop: real dogfood testing begins.

## Explicitly out of this pass

- Full PRIOS Tier 2 per-command test suite
- Doctor phase 2 (backend `list`, orphans, `--repair`)
- Multi-user, KEK rotation, recovery kit
- Serve deferred items (redb encryption, delta sync, WAL, benchmarks)
- Public marketing release post
