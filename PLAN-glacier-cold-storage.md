# PLAN: S3 cold storage (Glacier Deep Archive)

## Goal

Support a massive media vault on Amazon S3 with **Glacier Deep Archive**
(or similar archive classes) for content-addressed blobs, while keeping
catalog and key material instantly available. Cooling is infrastructure.
Thawing is product. Steady-state restore must not require manual
`aws s3` steps.

## Non-goals

- Auto-promote Deep Archive to Standard permanently on access (that is
  not how GDA works)
- Transitioning `indexes/` or `keys/` to any archive class
- Writing new blobs straight to DEEP_ARCHIVE
- Dual hot/cold backends as the primary design (named backends + mirror
  remain available for other use cases)
- Applying bucket lifecycle from every backup path

## Critical model (must stay true)

Access does **not** permanently warm Glacier Deep Archive.

| Model | On access |
| --- | --- |
| Lifecycle to DEEP_ARCHIVE | GET fails until `RestoreObject`. Temporary copy for N days, then cold again |
| Intelligent-Tiering Deep Archive Access | Access initiates restore; still hours; monitoring fee |
| Glacier Instant Retrieval | Milliseconds; no thaw; higher storage cost |

For rare bulk media restore at minimum $/TB: **Deep Archive + lifecycle +
blu thaw**. Instant Retrieval is the alternative when wait time is
unacceptable.

Cost invariants:

- 180-day minimum billable duration on Deep Archive
- Early delete/overwrite of GDA objects still incurs charges
- Retrieval and request fees dominate if thaw is frequent
- `defrag-blobs` / GC / repack that rewrites cold objects is expensive
- v3 range GETs work only after a temporary restore; thaw whole blob
  objects

## Current state

`src/storage/s3.rs` is Standard-only:

- `PutObject` with no storage class and no tags
- `GetObject` / range GET with no restore handling
- `HeadObject` used only for exists/not-found
- No `RestoreObject`

Backend layout under the vault prefix:

- Blobs (cold candidates): content-addressed sharded paths via
  `storage::path_for` (e.g. `d/dd4/dd4ce/...`), ~64 MiB packs
- Catalog / keys (must stay hot): `indexes/*`, `keys/*`

`S3Config` today: `bucket`, optional `prefix`, optional `region`.

## Design

### What cools vs what stays hot

- **Cool (lifecycle after N days):** content-addressed blob objects only
- **Hot forever:** `indexes/*` and `keys/*` (and any future catalog paths)

If indexes or KEK wrappers go to Deep Archive, `open` / `pull` / `doctor`
become multi-hour failures.

### Cooling (bucket-side)

1. Upload blobs as STANDARD (or STANDARD_IA) so backup and verify work
   immediately.
2. After operator-chosen delay (e.g. 30–90 days), lifecycle transitions
   tagged blobs to DEEP_ARCHIVE.
3. Lifecycle never matches catalog/key objects.

**Lifecycle filter:** object tags on put (preferred with current root
sharded blob paths; avoids 16 hex-prefix rules and avoids a breaking
`blobs/` prefix move).

- Blobs: `blu-role=blob`
- Indexes and keys: `blu-role=catalog` (or equivalent)

Optional later cleanup: store blobs under a `blobs/` prefix for simpler
prefix-only lifecycle rules. Not required for v1.

Lifecycle configuration is operator-owned (console, Terraform, or a
one-shot helper that prints/applies JSON). Do not re-apply lifecycle on
every backup.

### Thaw (blu-side)

Extend the S3 backend (and `BackendKind` where useful):

1. `stat_object` (or richer head): storage class, restore status
   (`ongoing-request`, expiry)
2. `restore_object`: `RestoreObject` with tier (`Bulk` vs `Standard`) and
   days
3. Map S3 `InvalidObjectState` (and related) to a typed error such as
   `BluError::ObjectArchived { path, class, restore }`

Object states:

```
Hot        -> GET works
Archived   -> must RestoreObject
Restoring  -> wait / poll
Restored   -> GET works until restore expiry, then Archived again
```

Thaw whole blob objects (dedup-friendly: one thaw serves many files).

### User-facing UX

Integrate with existing restore; do not invent a second storage product.

```text
blu thaw --path "photos/2024/**"   # unique blobs for selection; start restores
blu thaw --status                  # in-progress / ready / expired
blu restore --path "..."           # cold: fail fast with thaw hint (default)
blu restore --path "..." --thaw    # start thaw if needed, then restore if hot
blu restore --path "..." --wait    # optional: poll until ready, then restore
```

Semantics:

- Default `restore` on archived blobs: fail fast with count, ETA guidance,
  and `blu thaw` hint
- `thaw` is idempotent (do not re-fire RestoreObject if already restoring
  or temporarily restored)
- Temporary restore window default: 7–14 days (configurable)
- No permanent promote after access

`blu serve`: map archived backend reads to a clear client error (e.g.
503-style), never block for hours on GET.

### Config (optional fields on S3 backend)

```toml
[backends.media]
type = "s3"
bucket = "blu-media-vault"
region = "us-west-2"
# restore_tier = "bulk"           # bulk | standard
# restore_days = 14
# put_storage_class = "STANDARD"  # writes stay hot; lifecycle cools
```

Credentials remain environment / IAM only.

### Alternatives (documented, not primary)

| Choice | When |
| --- | --- |
| Deep Archive + thaw (this plan) | Huge media, rare access, min $/TB |
| Glacier Instant Retrieval | Occasional restore, no wait, higher storage $ |
| Intelligent-Tiering | Hands-off mixed access; still async for deep tier + monitoring fee |
| Second cold backend + `backend mirror` | Explicit hot/cold copies; more ops and storage $ |

## Stages

Stage 1: Design note in `docs/design/` (cold storage model, tagging,
lifecycle recipe, cost invariants, non-goals)

Stage 2: S3 put tagging (`blu-role=blob|catalog`) plus `stat` /
`restore` / archived error mapping in `storage` (and `BluError` as needed)

Stage 3: Blob-set planner: file/path selection to unique blob keys that
need thaw

Stage 4: `blu thaw` (+ status) and `restore` fail-fast / `--thaw` /
optional `--wait`

Stage 5: `blu doctor` cold-storage checks (indexes/keys hot; sample or
scan blob classes; restore status summary)

Stage 6: Bucket lifecycle docs and optional `blu backend lifecycle print`
(emit JSON; apply remains operator-owned unless a later explicit apply
command is requested)

Stage 7: `blu serve` maps archived backend reads to a clear client error

## Commit discipline

One atomic, reviewable commit per stage. Suggested subjects (imperative,
<= 50 chars, no plan-stage references):

1. `Document S3 cold storage design`
2. `Add S3 archive stat and restore APIs`
3. `Plan blob sets for cold thaw`
4. `Add thaw command and restore hooks`
5. `Detect cold storage in doctor`
6. `Document S3 lifecycle for archive`
7. `Surface archived blobs in serve`

## Open preference (defaults if unset)

- Target archive class: **DEEP_ARCHIVE** (cheapest; multi-hour thaw)
- Restore tier default: **Bulk** for cost; document Standard for faster
  single-day restores
- Restore days default: **14**
- Lifecycle delay: **documented recommendation only** (e.g. 90 days); not
  hard-coded into blu

If Instant Retrieval is preferred later, Stage 6 lifecycle target and
Stage 4 UX simplify (no thaw wait path required for that class).
