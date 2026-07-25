# PLAN: Backup UX cleanup after Zeal media run

## Context

First full media backup on zeal (`/Users/media`, ~187 GiB, 938 files) to
`s3://blackcarrot.de/bludata/media` finished successfully in 5h 24m on
`blu 0.8.1` with S3 upload backpressure (max 8 in flight). Catalog and
remote are in sync.

An earlier attempt without backpressure failed with
`S3 error: dispatch failure` at ~247 blobs in flight and `uploaded 0 B`.
That failure mode is already fixed (`PLAN-s3-upload-backpressure`,
merged). This plan covers remaining UX and observability issues exposed
by the successful run and by `blu backup -vv` pane capture.

## Problem

1. Overall progress total is `index_bytes + encrypt_bytes`, so a ~187 GiB
   tree shows ~373 GiB. The bar hits 100% when hashing finishes and then
   sits there for hours of encrypt+upload. Operators read the number as
   "how much data is in the vault" and think something is wrong.

2. Completion line on master still prints
   `N files indexed` where N is only new plain-index map entries
   (`after_len - before_len`). After a failed first run that already
   wrote the plain index, the successful re-run reported
   `1 files indexed, 383011 chunks encrypted` while the catalog holds
   938 files. Misleading, not a data-loss bug.
   (`incremental-index-skip` already improves hashed/skipped wording;
   this plan assumes that lands or restates the same summary rules.)

3. `blu backup -vv` enables full `debug` tracing. Per-chunk
   `debug!("Added chunk to memory!")` and AWS SDK/hyper DEBUG flood the
   terminal, exhaust tmux scrollback (~2000 lines retained only the last
   ~2 minutes of a 5+ hour run), and surface AWS access key ids via SDK
   identity debug lines. Useless for operators; harmful for support
   dumps.

4. Encrypt+upload phase has no sustained rate or ETA. For multi-hour
   network-bound runs the only signal is chunk/blob counters and
   `uploaded` bytes.

## Goals

- Honest overall progress that does not double-count the same plaintext
- Completion summary that always states catalog size (files and bytes),
  not only the delta of new map entries
- Verbose logging that is useful without drowning the session or leaking
  credential identifiers
- Optional upload rate (and simple ETA) on the encrypt+upload phase line

## Non-goals

- Changing chunking, encryption, envelope format, or S3 key layout
- Multipart upload, parallel object parts, or retuning
  `DEFAULT_MAX_INFLIGHT_UPLOADS` (leave at 8 unless a later plan measures
  otherwise)
- Full-screen TUI, structured JSON log mode, or remote telemetry
- GC of orphan sealed-but-unindexed S3 objects from the failed first run
- Fixing `blu doctor` hang observed on zeal (separate investigation)

## Design notes

### Progress total

Today (`BackupUiState` in `src/cli/progress.rs`):

- `overall_total = index_bytes_total + encrypt_bytes_total`
- `overall_done = index_bytes_done + encrypt_bytes_done`

Index and encrypt both walk the same plaintext byte mass on a cold
first backup, so the bar is ~2x `du`. Prefer one of:

- **Preferred:** overall bar tracks a single ordered work budget:
  index phase owns 0..index_bytes, then encrypt phase reuses the same
  0..encrypt_bytes scale only while that phase is active (bar resets
  length on phase enter), OR
- **Alternative:** keep a continuous bar but weight phases so totals
  reflect wall-clock intent (e.g. overall = index_bytes + encrypt_bytes
  only when both are planned, but display primary "data size" separately
  as catalog/source bytes, never as the sum label)

Whichever option, the primary numeric readout next to the bar must not
read like "vault is 373 GiB" when source is 187 GiB. Show source/catalog
bytes once; show phase work distinctly.

Progress must still hit a true 100% only when finalize and push indexes
complete (or keep a non-percent phase spinner for post-encrypt work so
"100%" is not shown while uploads remain).

### Completion summary

Always print durable facts after a successful backup:

- files hashed / unchanged skipped / new content hashes (if skip work
  has landed)
- chunks encrypted this run
- `Index contains N files (human bytes) total`
- `Blob index contains M blob files`
- bytes uploaded this run (if cheap to plumb from existing events)

Never imply the vault only holds `files_added` paths.

### Logging

- Remove or demote to `trace!` the per-chunk
  `Added chunk to memory!` and similarly chatty seal lines in
  `src/blob.rs`
- Keep `info!` for rare milestones (push index paths, fatal upload
  failure already printed on the progress UI)
- `-v` / `-vv` should raise blu crate verbosity without forcing
  `aws_sdk_*=debug` / `hyper=debug` by default; gate SDK noise behind
  an explicit knob (e.g. `-vvv` or `RUST_LOG`) so access key ids do not
  appear in normal verbose operator sessions

### Rate / ETA

On encrypt+upload phase detail, add a short sustained rate from
`uploaded` byte deltas over a sliding window (e.g. last 10-30s), and an
ETA from remaining encrypt plaintext or remaining sealed-not-uploaded
bytes. Best-effort only; hide when sample is too small. No new
dependencies if `indicatif` already covers this.

## Stages

Stage 1: Fix overall progress double-count
  1a: Change `BackupUiState::overall_total` / `overall_done` (or phase
      bar length policy) so a cold backup of size S does not show ~2S
  1b: Ensure the bar does not read 100% while finalize or push indexes
      still run (extend total, or clear percent for those phases)
  1c: Unit tests on `BackupUiState` / progress apply path covering
      index-then-encrypt totals and post-encrypt phases

Stage 2: Honest completion summary
  2a: Always print catalog file count and total bytes after backup
  2b: Align wording with hashed/skipped/new if
      `incremental-index-skip` is merged; otherwise keep master fields
      but stop calling the map delta "files indexed" alone
  2c: Include blob file count and uploaded bytes this run when available
  2d: Smoke or unit coverage that a re-run with a warm plain index still
      prints the full catalog size

Stage 3: Tame verbose logging
  3a: Demote or delete per-chunk / per-seal debug spam in `blob.rs`
  3b: Stop mapping `-vv` to global AWS SDK debug; document `RUST_LOG`
      (or add `-vvv`) for SDK-level traces
  3c: Confirm a short backup under `-vv` no longer prints access key ids
      from identity debug lines

Stage 4: Upload rate and ETA on phase line
  4a: Track uploaded byte samples in the progress UI task
  4b: Show MiB/s (and optional ETA) on encrypt+upload detail
  4c: Hide rate/ETA until enough samples exist; no panic on idle

## Acceptance

- Cold backup of a known tree size S shows overall work on the order of
  S (not ~2S) during index and during encrypt
- Successful backup summary states full catalog file count matching
  `blu status`
- `blu backup -vv` on a small vault remains readable and does not emit
  AWS access key ids
- Multi-minute encrypt+upload shows a non-zero rate once uploads flow
- Existing backup smokes and clippy/fmt stay green

## References

- Zeal tmux session `media:0` capture after 5h24m success (2026-07-24)
- `/Users/media/hist` earlier failure: 246 in flight, dispatch failure
- `src/cli/progress.rs` (`overall_total`, phase detail)
- `src/cli/backup.rs` (completion `println!`s)
- `src/blob.rs` (`debug!("Added chunk to memory!")`,
  `DEFAULT_MAX_INFLIGHT_UPLOADS`)
- Archived: `docs/plans/archived/PLAN-backup-progress.md`,
  `docs/plans/archived/PLAN-s3-upload-backpressure.md`
