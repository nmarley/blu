> Archived plan — fully implemented. See commit history and
> `../../design/S3_COLD_STORAGE_DESIGN.md` for the canonical reference.

# PLAN: Cold storage follow-up hardening

Follow-up work for the S3 Intelligent-Tiering cold storage feature (see
`PLAN-glacier-cold-storage.md` and `../../design/S3_COLD_STORAGE_DESIGN.md`).
Lands as atomic commits on the `s3-glacier` branch.

## Goal

Tighten the shipped cold storage path before the greenfield media vault
is seeded: final blob layout, thaw robustness, live verification of the
restore request shapes, and bounds on whole-index scans so the design
holds at massive-vault scale.

## Non-goals

- Migration or re-tagging of pre-existing objects (greenfield redo; the
  vault will be recreated from scratch)
- Changing the shipped model (blob puts `INTELLIGENT_TIERING`, catalog
  puts `STANDARD`, Deep Archive Access at 365 days, operator-applied
  bucket configuration)
- Auto-applying bucket Intelligent-Tiering configuration from blu
- Lifecycle `DEEP_ARCHIVE` storage class, Object Lock / WORM
- Removing the restore cold preflight (cost is documented; an escape
  hatch is added only if measurement shows real pain)

## Current state

Shipped on this branch: tag/class puts, `stat_object` / `restore_object`
backend APIs, thaw planner plus `blu thaw`, restore `--thaw` / `--wait`,
doctor cold checks, `backend intelligent-tiering print`, and serve
`InvalidObjectState` mapping.

Known rough edges:

- Blobs live at root-sharded paths (`d/dd4/...`); bucket filters are
  tag-only, which is weaker than prefix scoping
- Doctor `blob-cold-status` and bare `blu thaw --status` HEAD every
  indexed blob, which gets spendy at tens of thousands of blobs
- `wait_until_readable` loops forever on missing blobs and on
  persistent stat errors when no timeout is set
- `initiate_thaw` classifies, then `restore_object` re-stats each blob
  (two HEADs per thawed object)
- Wait polling is a fixed 30s, noisy for multi-hour Deep Archive
  restores
- Serve archive preflight HEADs blobs serially per file
- The Intelligent-Tiering archive-tier `RestoreObject` shape (tier
  only, no days) is unverified against live S3

## Stages

Stage 1: Store blobs under a `blobs/` backend prefix
  1a: `storage::path_for` / `hash_from_path` move blobs from
      root-sharded (`d/dd4/...`) to prefix-sharded
      (`blobs/d/dd4/...`); update `is_non_blob_prefix` and
      `list_blob_paths` to scope listing under `blobs/`
  1b: `backend intelligent-tiering print` emits an
      `And(prefix, tag)` filter (prefix-only filtering becomes
      possible; the tag stays as defense in depth)
  1c: Update `AGENTS.md`, `docs/design/S3_COLD_STORAGE_DESIGN.md`, and
      path-shape tests. `PLAN-glacier-cold-storage.md` stays untouched
      (historical record)

Stage 2: Harden thaw wait and restore probes
  2a: `wait_until_readable` treats missing blobs as terminal errors and
      bails after a bounded number of consecutive stat errors (no
      infinite error loops when no timeout is set)
  2b: `restore_object` accepts an optional prior stat so
      `initiate_thaw` does not double-HEAD every blob
  2c: Remove the unreachable `GLACIER_IR` arm in
      `build_restore_request`

Stage 3: Verify restore request shapes against live S3
  3a: Extend the ignored live S3 test: put an `INTELLIGENT_TIERING`
      object and confirm `restore_object` maps the already-active
      response to `Ok` (proves the tier-only request serializes
      correctly); put a `GLACIER`-class object and run a real Bulk
      restore to completion, then GET
  3b: Runbook in the test comments (credentials, bucket, multi-hour
      wait). Results recorded in the design doc at Stage 9

Stage 4: Bound doctor and status cold scans
  4a: `blob-cold-status` samples (capped count, reported as sampled)
      instead of HEADing every indexed blob
  4b: Bare `blu thaw --status` on a large index shares the cap or asks
      for explicit confirmation
  4c: Doctor verifies the bucket actually has the blu
      Intelligent-Tiering configuration
      (`GetBucketIntelligentTieringConfiguration`; warn-only when IAM
      denies); IAM docs gain `s3:GetIntelligentTieringConfiguration`

Stage 5: Back off cold wait polling
  5a: `wait_until_readable` takes an initial interval and a max cap
      (30s exponential up to 5min) instead of a fixed 30s; `thaw_cmd`
      and `restore` pass the new parameters

Stage 6: Parallelize serve archive preflight
  6a: `ensure_file_blobs_readable` runs through `thaw::classify_blobs`
      with a small semaphore and maps the first blocked blob to
      `ObjectArchived` (replaces the serial per-blob HEAD loop)

Stage 7: Document the hot-restore preflight tradeoff
  7a: Measure HEAD overhead on a large hot restore; record the decision
      (keep always-on unless measured pain) in the design doc. Likely
      docs-only

Stage 8: Optional Archive Access tier in `intelligent-tiering print`
  8a: `--archive-days N` emits a second `Tierings` entry (90..=730,
      must be less than the Deep Archive days); validation plus tests

Stage 9: Changelog and cold storage docs
  9a: CHANGELOG entry for the cold storage feature set
  9b: Design doc updates: `blobs/` layout, live verification results,
      preflight decision, scan bounds

## Commit discipline

One atomic, reviewable commit per stage. Suggested subjects
(imperative, <= 50 chars, no plan-stage references):

1. `Store blobs under blobs/ backend prefix`
2. `Harden thaw wait and restore probes`
3. `Verify restore request shapes on live S3`
4. `Bound doctor cold scans; check bucket IT config`
5. `Add backoff to cold wait polling`
6. `Parallelize serve archive preflight`
7. `Document restore preflight cost tradeoff`
8. `Add optional Archive Access tier to IT print`
9. `Update changelog and cold storage docs`

## Open decisions

- Sample size and strategy for bounded scans (first-N vs random)
- Whether bare `blu thaw --status` gets the same cap or a confirmation
  prompt
- Escape hatch for the restore cold preflight, only if Stage 7
  measurement shows real overhead
