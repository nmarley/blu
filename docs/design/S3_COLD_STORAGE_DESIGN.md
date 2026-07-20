# S3 Intelligent-Tiering cold storage

Canonical design for cooling blu vault blobs on Amazon S3 with
**S3 Intelligent-Tiering**, ending in **Deep Archive Access after 365
consecutive days of no access**. Catalog and key material stay
instantly available. Cooling is infrastructure. Thawing is product.

## Status

**Shipped** for the cold-storage product path (puts, thaw, restore
hooks, doctor, Intelligent-Tiering print, serve InvalidObjectState).
This document remains the design reference.

## Goals

1. Cheap long-term storage for large, rarely restored media vaults
2. Access-driven cooling (unused blobs get cheaper; used blobs re-warm)
3. Catalog and KEK material always available without multi-hour waits
4. Restore UX that does not require manual `aws s3` as the steady state
5. Doctor and status scans that do not accidentally re-warm cold objects

## Non-goals

- Lifecycle transition of the whole bucket to the `DEEP_ARCHIVE` storage
  class as the primary design (one-way cold, not access-driven)
- Putting `indexes/` or `keys/` into Archive Access or Deep Archive
  Access tiers
- Writing new blobs as the standalone `DEEP_ARCHIVE` storage class
- Dual hot/cold backends as the primary model (named backends and
  `backend mirror` remain available for other use cases)
- Re-applying bucket Intelligent-Tiering configuration on every backup
- Object Lock / retention (WORM). Different AWS product; not this design

## Why Intelligent-Tiering (not lifecycle GDA)

| Approach | Behavior | Fit for blu media vault |
| --- | --- | --- |
| **Intelligent-Tiering** (primary) | Storage class stays `INTELLIGENT_TIERING`. AWS moves objects between access tiers by consecutive days without access. Restore from archive tiers returns the object to Frequent Access. | Matches "cool if untouched for months; warm again if restored" |
| Lifecycle to `DEEP_ARCHIVE` | One-way storage class change. Restore creates a temporary copy that expires; object does not permanently re-warm. | Worse UX for occasional restores; keep as documented alternative only |
| Put as `DEEP_ARCHIVE` | Immediate deep cold on write | Breaks verify, multi-device pull of fresh data, and any near-term restore |

Object Lock "retention" is unrelated. The AWS name for the idle-to-cold
settings is **Intelligent-Tiering archive configuration** (optional
Archive Access and Deep Archive Access tiers).

## Access tiers

Objects keep storage class **`INTELLIGENT_TIERING`**. AWS moves them
between access tiers:

| Access tier | When | GET? |
| --- | --- | --- |
| Frequent Access | Default; recently used | Instant |
| Infrequent Access | 30 days no access (automatic) | Instant; access moves back to Frequent |
| Archive Instant Access | 90 days no access (automatic) | Instant; access moves back to Frequent |
| Archive Access (optional) | Configurable 90–730 days no access | Restore first (~3–5h standard) |
| Deep Archive Access (optional) | **365 days no access (this design)** | Restore first (~12h standard) |

**Counts as access** (resets the no-access clock; re-warms Infrequent
and Archive Instant to Frequent): `GetObject`, `PutObject`,
`RestoreObject`, copy, console download, and similar.

**Does not count as access** (safe for doctor / status): `HeadObject`,
list, get/put object tags.

After a successful restore from Archive Access or Deep Archive Access,
AWS returns the object to **Frequent Access**. The year clock toward
Deep Archive Access starts again from that point.

Objects under 128 KiB never leave Frequent Access. Blu content-addressed
blobs are on the order of tens of MiB (default pack ~64 MiB), so they
are fully eligible for tiering.

## What cools vs what stays hot

Backend layout under the vault prefix (today):

- **Blobs** (tiering candidates): content-addressed paths from
  `storage::path_for` (e.g. `d/dd4/dd4ce/...`)
- **Catalog / keys** (must stay hot): `indexes/*`, `keys/*`

| Object role | Put storage class | Object tag | Intelligent-Tiering archive filter |
| --- | --- | --- | --- |
| Content-addressed blob | `INTELLIGENT_TIERING` | `blu-role=blob` | Included |
| Index / KEK / catalog | `STANDARD` | `blu-role=catalog` | Excluded |

If indexes or KEK wrappers enter Deep Archive Access, `open`, `pull`,
and `doctor` become multi-hour failures. That must never happen by
default.

Tagging is preferred for filters with the current root-sharded blob
paths (avoids sixteen hex-prefix rules). A future `blobs/` prefix is an
optional layout cleanup, not required for the first implementation.

## Bucket configuration (infrastructure)

Operator-owned, applied once per bucket (console, Terraform, or a
one-shot helper that prints JSON):

1. Intelligent-Tiering configuration with filter on tag
   `blu-role=blob` (and optional vault prefix)
2. Deep Archive Access after **365** consecutive days of no access
3. Archive Access tier optional (document; not required for v1 minimum)
4. Do not re-apply this configuration from every `blu backup`

Emit the configuration JSON with:

```sh
blu backend intelligent-tiering print
# optional: --backend NAME --days 365 --id blu-blobs-deep-archive
```

Applying it remains operator-owned (console, Terraform, or `aws s3api`).
Blu does not re-apply this on backup.

### Operator setup (one-time per bucket)

1. Create the S3 bucket (or reuse one). Prefer a dedicated media vault
   bucket or a unique key prefix per vault.
2. Ensure IAM can `s3:PutObject`, `GetObject`, `HeadObject`,
   `DeleteObject`, `ListBucket`, `RestoreObject`, and
   `s3:PutIntelligentTieringConfiguration` (apply only).
3. Point the vault at the bucket (`blu backend add` / `blu open --type s3`).
4. Print and apply Intelligent-Tiering archive config for **blobs only**:

```sh
blu backend intelligent-tiering print > blu-it-config.json
aws s3api put-bucket-intelligent-tiering-configuration \
  --bucket YOUR_BUCKET \
  --id blu-blobs-deep-archive \
  --intelligent-tiering-configuration file://blu-it-config.json
```

5. Confirm: new blobs upload as `INTELLIGENT_TIERING` with tag
   `blu-role=blob`; indexes/keys as `STANDARD` with `blu-role=catalog`.
6. After long idle (365 days no access on a blob), Deep Archive Access
   may apply. Use `blu thaw` / `blu restore --thaw` before materializing
   plaintext. `blu doctor` reports `catalog-hot` and `blob-cold-status`.

Do **not** put a whole-bucket lifecycle rule to the `DEEP_ARCHIVE`
storage class; that can cold-store catalog objects and is not the
access-driven model this design uses.

## Blu product model (application)

### Put path

- Blobs: `PutObject` with `StorageClass=INTELLIGENT_TIERING` and tag
  `blu-role=blob`
- Indexes and keys: `PutObject` with `StorageClass=STANDARD` and tag
  `blu-role=catalog`
- Credentials stay environment / IAM only (no secrets in config)

### Read path and archive errors

Even with Intelligent-Tiering, objects in Archive Access or Deep Archive
Access require `RestoreObject` before `GetObject` (including range GET).

Backend capabilities to add:

1. Richer head / `stat_object`: storage class, archive or restore status
2. `restore_object`: initiate restore (Bulk vs Standard as applicable)
3. Typed error for archived objects (e.g. `BluError::ObjectArchived`)
   instead of a generic S3 string

States for async archive tiers:

```
Hot (Frequent / IA / Archive Instant) -> GET works
Archived (Archive Access / Deep Archive Access) -> must RestoreObject
Restoring -> wait / poll
Restored (back in Frequent) -> GET works; no-access clock resets
```

Thaw **whole blob objects**. Dedup means one thaw can serve many files.
v3 segmented range reads only work after the object is available again.

### CLI UX

Integrate with existing restore; do not invent a second storage product.

```text
blu thaw --path "photos/2024/**"   # unique blobs for selection; start restores
blu thaw --status                  # in-progress / ready / not needed
blu restore --path "..."           # cold: fail fast with thaw hint (default)
blu restore --path "..." --thaw    # start thaw if needed, then restore if hot
blu restore --path "..." --wait    # optional: poll until ready, then restore
```

Semantics:

- Default `restore` on archived blobs: fail fast with count, ETA guidance,
  and a `blu thaw` hint
- `thaw` is idempotent (do not re-fire RestoreObject if already restoring
  or already available)
- After IT archive restore completes, the object is in Frequent Access
  again; further access keeps it warm and resets the 365-day clock
- Status and doctor prefer `HeadObject` so scans do not re-warm or reset
  archive timers

### Config (optional S3 backend fields)

```toml
[backends.media]
type = "s3"
bucket = "blu-media-vault"
region = "us-west-2"
# put_storage_class = "INTELLIGENT_TIERING"  # blobs only; catalog stays STANDARD
# restore_tier = "bulk"                      # bulk | standard
# restore_days = 14                          # if the restore API requires days
```

Defaults when unset: Intelligent-Tiering for blobs, Bulk restore tier,
sensible restore days if required by the API.

### `blu serve`

Map archived backend reads to a clear client error. Never block an HTTP
GET for hours waiting on Deep Archive Access.

### `blu doctor`

Cold-storage checks (when implemented):

- Indexes and keys are STANDARD and immediately readable
- Sample or scan blob archive status without using GET
- Summarize archived / restoring / hot blob counts where practical

## Cost and ops invariants

- Monitoring and automation fee applies per Intelligent-Tiering object
  (~$0.0025 per 1,000 objects). At ~64 MiB blobs this is small relative
  to multi-TB storage.
- Most savings for long-idle media come from Infrequent Access and
  Archive Instant Access. Deep Archive Access after a year is still
  worth enabling for rarely touched bulk; it is not expected to
  dominate savings over the tier immediately above it.
- Retrieval and request fees dominate if deep archive thaw is frequent.
  This design assumes rare bulk restore, not casual browsing of cold data.
- `defrag-blobs`, GC, and repack that rewrite objects create new puts
  (and new no-access clocks). Prefer not to churn cold media without
  need. Rewrites of objects that were in deep archive tiers also incur
  retrieval-related cost if they must be read first.
- Early delete of objects that spent time only in automatic IT tiers is
  not the same billing model as the standalone `DEEP_ARCHIVE` storage
  class minimum duration; still avoid pointless rewrite churn.

## Alternatives (documented only)

| Choice | When to consider |
| --- | --- |
| Lifecycle to `DEEP_ARCHIVE` storage class | Fixed one-way cold; no automatic re-warm to Frequent |
| Glacier Instant Retrieval as put class | Occasional restore, no multi-hour wait, higher storage $ |
| Second cold backend + `backend mirror` | Explicit hot/cold copies; more ops and storage $ |

## Implementation outline

Order of work (each unit one reviewable commit):

1. This design document (shipped)
2. S3 put tagging and storage class; `stat` / `restore` / archived errors (shipped)
3. Blob-set planner: path/file selection to unique blob keys needing thaw (shipped)
4. `blu thaw` (+ status) and `restore` fail-fast / `--thaw` / optional `--wait` (shipped)
5. Doctor cold-storage checks (shipped)
6. Docs and `blu backend intelligent-tiering print` (shipped)
7. `blu serve` clear errors for archived backend reads (shipped)

## Locked preferences

- Primary cooling model: S3 Intelligent-Tiering
- Blob put class: `INTELLIGENT_TIERING`
- Catalog/key put class: `STANDARD`
- Object tags: `blu-role=blob` | `blu-role=catalog`
- Deep Archive Access: **365** consecutive days of no access
- Archive Access tier: optional; document, not required for v1 minimum
- Restore tier default: Bulk (document Standard for faster restores)
