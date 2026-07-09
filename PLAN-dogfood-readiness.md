# blu dogfood readiness plan (0.5.0)

Static design document for one final pass before real alpha / dogfood
testing. Scope: CLI surface cleanup, `.bluignore`, `blu doctor` (phase 1),
CLI smoke tests, GitHub Actions CI, docs truth-up, and changelog.

Decisions locked:

- Crate version stays **0.5.0**. ROADMAP is reframed as internal 0.5.x
  pre-release milestones (no re-tag to v0.1.0-alpha).
- CLI tests: smoke-level only (happy path + one doctor failure path).
  Full PRIOS Tier 2 per-command suite is deferred.
- `blu doctor`: phase 1 only (no `BackendKind::list`, no orphan scan,
  no `--repair`).
- Plumbing commands stay invokable but hidden from help; obsolete
  `debug-index` is removed.
- `.bluignore` is gitignore-style via the `ignore` crate. Do not port
  the ancient `bluignore` branch (salvage intent only).

Progress is recorded by git history and the working conversation, not
by mutating this file.

## Stage 1: CLI surface cleanup

1a. Hide plumbing from help: `write-index`, `encrypt-files`, `read-index`
    (`hide = true` in clap; keep invokable).
1b. Delete obsolete `debug-index` command (handler, clap variant, dispatch).
1c. `cargo test` + `cargo clippy` clean.

## Stage 2: `.bluignore` (gitignore-style)

2a. Add the `ignore` crate; new shared walk module (`src/ignore.rs` or
    equivalent) using `WalkBuilder`.
2b. Rules: always exclude `.blu/` and `.git/`; load vault-root
    `.bluignore` with gitignore semantics (`#`, globs, `!`, trailing
    `/` for dirs). Nested `.bluignore` files are fine if free via the
    crate.
2c. Explicit single-file CLI path overrides ignore (git-style). Directory
    walks honor ignore.
2d. Replace walkers in `PlainIndex::add` and `status::get_files_and_sizes`
    with the shared walker so add/sync/status agree.
2e. Tests: tempdir patterns, nested dirs, `.blu/` still excluded, ignored
    files not indexed by add/sync, explicit path override.

## Stage 3: `blu doctor` (phase 1)

3a. Clap + `src/cli/doctor.rs` + dispatch.
3b. Checks (report pass/warn/fail; exit non-zero on any fail):

- config readable, backends non-empty, default exists
- encryption configured (`pq_recipient`)
- KEK / key material present under `.blu/`
- agent reachable + unlock state (warn if daemon down when not required)
- plain / blob / tag indexes decrypt
- plain index version vs current
- internal cross-refs (chunks ↔ files, no empty path sets)
- encryption coverage (unencrypted chunks as warn/info)
- every indexed blob path: `backend.exists`
- pending GC: `paths_to_delete` / `paths_to_repack` counts (info unless
  inconsistent)

3c. No `BackendKind::list`, no orphan scan, no `--repair` this pass.
3d. Tests with temp vault: healthy vault passes; missing blob or
    corrupt-ref fails.

## Stage 4: End-to-end CLI smoke tests

4a. Inline `#[cfg(test)]` smokes via handler/library APIs on tempdirs
    (not a full assert_cmd suite).
4b. Happy path: identity (or test fixtures) → init → sync small tree →
    status → list → restore → delete → doctor clean.
4c. One failure path: doctor fails when an indexed blob is missing from
    the backend.
4d. Keep fast; no scrypt-heavy cases unless already `#[ignore]`.

## Stage 5: GitHub Actions CI

5a. `.github/workflows/ci.yml` on push and pull_request.
5b. Matrix: `macos-latest` (primary) and `ubuntu-latest`.
5c. Steps: checkout, stable Rust, `cargo build`, `cargo test`,
    `cargo clippy -- -D warnings`, `cargo fmt -- --check`.
5d. No release or publish workflow this pass.

## Stage 6: User-facing docs (README + changelog + crate docs)

6a. Rewrite `README.md`:

- Crypto truth: PQ hybrid UK (ML-KEM-768 + X25519), KEK wrap via age,
  bulk data ChaCha20-Poly1305 (not "all rage / classic X25519 age")
- Identity-first quick start: `identity init` → `unlock` → `init` →
  `sync`
- Named multi-backend config example (`default_backend` +
  `[backends.<name>]`)
- Full user command table: identity, unlock/lock/agent, backend,
  serve, delete-files, defrag-blobs, doctor
- Document `.bluignore`

6b. Fix `src/lib.rs` crate docs (same crypto story as README).
6c. Add `CHANGELOG.md` (Keep a Changelog format) with initial
    `[0.5.0]` entry describing the shipped dogfood surface.

## Stage 7: Design + project docs truth-up

7a. `docs/design/ENVELOPE_ENCRYPTION_DESIGN.md`: PQ-first hierarchy;
    fix salts (`blu-pq-v1`, `blu-device-key-v1`); `identity.toml` fields;
    24-word mnemonic; `tags.dat`; label multi-user / KEK rotate /
    recovery-kit as future.
7b. `docs/design/BLU_SERVE_DESIGN.md`: v3 segmented as current for new
    writes; prefix-fetch in present tense; only Local + AmazonS3 as
    implemented backends.
7c. `AGENTS.md`: design-doc path fix; note ciborium indexes; clarify
    `test/blu_secrets/` if still classic age fixtures.
7d. `docs/project/START-HERE.md`: refresh date; version story (crate
    0.5.0 pre-release dogfood); note ignore, doctor, CI; update
    LOC/test counts.
7e. `docs/project/ROADMAP.md`: reframe milestones under 0.5.x; drop the
    v0.1.0-alpha naming conflict; update LOC/tests; path fixes; mark
    ignore/doctor/CI done once landed.
7f. `docs/project/PRIOS.md` and `TODO.md`: check off completed items;
    refresh dates.
7g. `docs/analyses/*`: banner HISTORICAL / SUPERSEDED (bincode analysis
    is fixed; session dump is archive-only).
7h. Move completed root `PLAN-serve-hardening.md` into `docs/plans/`
    (plan-doc housekeeping).

## Stage 8: Dogfood checklist (manual, not code)

8a. Fresh path: `identity init` → `unlock` → `init` → files +
    `.bluignore` → `sync` → `status` → `doctor` → `serve` smoke →
    `restore-files` → `delete-files` → `defrag-blobs`.
8b. Confirm CI green after push.
8c. Stop: real testing begins.

## Explicitly out of this pass

- Full PRIOS Tier 2 per-command test suite
- Doctor phase 2 (backend `list`, orphans, `--repair`)
- redb at-rest encryption, delta sync, v2/v3 benchmarks, crash-atomic WAL
- Multi-user, KEK rotation, recovery kit
- Feature-gating S3 / security-framework
- Public marketing release post

## Risk notes

- `.bluignore` must hit both walkers or status and sync will disagree.
- Doctor must not hard-require the agent for pure structural checks that
  only need local indexes when test helpers already supply keys.
- Linux CI may need to soft-skip Touch ID paths (already platform-gated;
  verify).
