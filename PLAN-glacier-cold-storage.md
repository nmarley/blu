# PLAN: S3 Intelligent-Tiering cold storage

## Goal

Support a massive media vault on Amazon S3 using **S3 Intelligent-Tiering**
so blobs automatically step down through cheaper access tiers when unused,
with the final optional tier **Deep Archive Access after 365 consecutive
days of no access**. Catalog and key material stay instantly available.
Cooling is infrastructure. Thawing is product. Steady-state restore must
not require manual `aws s3` steps.

## Non-goals

- Lifecycle transition of the whole bucket to the `DEEP_ARCHIVE` storage
  class as the primary design (that is one-way cold, not access-driven)
- Transitioning `indexes/` or `keys/` into Archive Access or Deep Archive
  Access tiers
- Writing new blobs as the standalone `DEEP_ARCHIVE` storage class
- Dual hot/cold backends as the primary design (named backends + mirror
  remain available for other use cases)
- Re-applying bucket Intelligent-Tiering configuration on every backup
- Object Lock / retention (WORM). That is a different AWS product.

## Critical model (must stay true)

Objects keep storage class **`INTELLIGENT_TIERING`**. AWS moves them between
**access tiers** inside that class based on consecutive days without access.

| Access tier | When (defaults / plan) | GET? |
| --- | --- | --- |
| Frequent Access | Default; recently used | Instant |
| Infrequent Access | 30 days no access (automatic) | Instant; access moves back to Frequent |
| Archive Instant Access | 90 days no access (automatic) | Instant; access moves back to Frequent |
| Archive Access (optional) | Configurable 90–730 days no access | Restore first (~3–5h standard) |
| Deep Archive Access (optional) | **365 days no access (this plan)** | Restore first (~12h standard) |

Access that counts (resets no-access clock; re-warms IA / Archive Instant
to Frequent): `GetObject`, `PutObject`, `RestoreObject`, copy, console
download, and similar.

Not access (safe for doctor / status): `HeadObject`, list, get/put tags.

After a successful restore from Archive Access or Deep Archive Access,
AWS returns the object to **Frequent Access**. Temporary lifecycle-style
“copy expires then stays cold forever” is **not** the IT model.

Objects under 128 KiB never leave Frequent Access. Blu blobs are ~64 MiB,
so they are fully eligible for tiering.

Monitoring and automation fee applies per object in Intelligent-Tiering
(~$0.0025 per 1,000 objects). At ~64 MiB blobs this is small relative to
storage for a large media vault.

Cost note: most savings for long-idle media come from IA and Archive
Instant Access. Deep Archive Access after a year is still worth enabling
for rarely touched bulk; it is not expected to dominate savings over the
tier immediately above it.

## Current state

`src/storage/s3.rs` is Standard-only:

- `PutObject` with no storage class and no tags
- `GetObject` / range GET with no restore handling
- `HeadObject` used only for exists/not-found
- No `RestoreObject`

Backend layout under the vault prefix:

- Blobs (tiering candidates): content-addressed sharded paths via
  `storage::path_for` (e.g. `d/dd4/dd4ce/...`), ~64 MiB packs
- Catalog / keys (must stay hot): `indexes/*`, `keys/*`

`S3Config` today: `bucket`, optional `prefix`, optional `region`.

## Design

### What tiers vs what stays hot

- **Intelligent-Tiering + archive configs:** content-addressed blob
  objects only (`blu-role=blob`)
- **Always STANDARD, never under archive filter:** `indexes/*` and
  `keys/*` (`blu-role=catalog`)

If indexes or KEK wrappers enter Deep Archive Access, `open` / `pull` /
`doctor` become multi-hour failures.

### Cooling (bucket-side)

1. Upload blobs as `StorageClass=INTELLIGENT_TIERING` with tag
   `blu-role=blob`.
2. Upload indexes and keys as `StorageClass=STANDARD` with tag
   `blu-role=catalog`.
3. Bucket **Intelligent-Tiering configuration** (not primary lifecycle
   rules) with a filter on `blu-role=blob`:
   - Optional Archive Access: enable if desired (e.g. 180 days); can
     document as optional for v1
   - **Deep Archive Access: 365 consecutive days of no access**
4. Configuration is operator-owned (console, Terraform, or a one-shot
   helper that prints/applies JSON). Do not re-apply on every backup.

Tagging is preferred for the filter with the current root-sharded blob
paths. Optional later cleanup: store blobs under a `blobs/` prefix for
simpler prefix-only filters. Not required for v1.

### Thaw (blu-side)

Even with Intelligent-Tiering, objects in Archive Access or Deep Archive
Access require `RestoreObject` before GET. Extend the S3 backend (and
`BackendKind` where useful):

1. `stat_object` (or richer head): storage class, archive status /
   restore status (`ongoing-request`, expiry / tier hints)
2. `restore_object`: `RestoreObject` with tier (`Bulk` vs `Standard`) as
   applicable to IT archive tiers
3. Map S3 `InvalidObjectState` (and related) to a typed error such as
   `BluError::ObjectArchived { path, class, restore }`

Object states for async tiers:

```
Hot (Frequent / IA / Archive Instant) -> GET works
Archived (Archive Access / Deep Archive Access) -> must RestoreObject
Restoring -> wait / poll
Restored / re-warmed to Frequent -> GET works; no-access clock resets
```

Thaw whole blob objects (dedup-friendly: one thaw serves many files).
v3 range GETs only work after the object is available again.

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
  or already available)
- After IT archive restore completes, object is back in Frequent Access;
  subsequent access keeps it warm and resets the year clock toward Deep
  Archive Access again
- `HeadObject`-based status / doctor must not count as access that
  re-warms or resets archive timers (AWS already treats Head as non-access)

`blu serve`: map archived backend reads to a clear client error (e.g.
503-style), never block for hours on GET.

### Config (optional fields on S3 backend)

```toml
[backends.media]
type = "s3"
bucket = "blu-media-vault"
region = "us-west-2"
# put_storage_class = "INTELLIGENT_TIERING"  # blobs only; catalog stays STANDARD
# restore_tier = "bulk"                      # bulk | standard
# restore_days = 14                          # if API still requires days for IT restore
```

Credentials remain environment / IAM only.

### Alternatives (documented, not primary)

| Choice | When |
| --- | --- |
| Intelligent-Tiering + Deep Archive Access @ 365d (this plan) | Large media vault, rare deep restores, access-driven cooling |
| Lifecycle to `DEEP_ARCHIVE` storage class | Fixed one-way cold; no automatic re-warm to Frequent |
| Glacier Instant Retrieval as put class | Occasional restore, no wait, higher fixed storage $ |
| Second cold backend + `backend mirror` | Explicit hot/cold copies; more ops and storage $ |

## Stages

Stage 1: Design note in `docs/design/` (Intelligent-Tiering model, tags,
archive configuration with Deep Archive Access at 365 days, cost
invariants, non-goals, distinction from lifecycle GDA and Object Lock)

Stage 2: S3 put tagging and storage class (`blu-role=blob` +
`INTELLIGENT_TIERING` for blobs; `blu-role=catalog` + `STANDARD` for
indexes/keys) plus `stat` / `restore` / archived error mapping in
`storage` (and `BluError` as needed)

Stage 3: Blob-set planner: file/path selection to unique blob keys that
need thaw

Stage 4: `blu thaw` (+ status) and `restore` fail-fast / `--thaw` /
optional `--wait`

Stage 5: `blu doctor` cold-storage checks (indexes/keys STANDARD and hot;
sample or scan blob classes / archive status; restore status summary;
prefer HeadObject so scans do not re-warm)

Stage 6: Bucket Intelligent-Tiering docs and optional
`blu backend lifecycle print` (or better name:
`blu backend intelligent-tiering print`) emitting the JSON for a filter on
`blu-role=blob` with Deep Archive Access at **365** days; apply remains
operator-owned unless a later explicit apply command is requested

Stage 7: `blu serve` maps archived backend reads to a clear client error

## Commit discipline

One atomic, reviewable commit per stage. Suggested subjects (imperative,
<= 50 chars, no plan-stage references):

1. `Document S3 Intelligent-Tiering design`
2. `Add S3 archive stat and restore APIs`
3. `Plan blob sets for cold thaw`
4. `Add thaw command and restore hooks`
5. `Detect cold storage in doctor`
6. `Document S3 Intelligent-Tiering setup`
7. `Surface archived blobs in serve`

## Locked preferences

- Primary cooling model: **S3 Intelligent-Tiering** (not lifecycle GDA)
- Blob put class: **INTELLIGENT_TIERING**
- Catalog/key put class: **STANDARD**
- Deep Archive Access: **365** consecutive days of no access
- Archive Access tier: optional / document; not required for v1 minimum
- Restore tier default: **Bulk** for cost; document Standard for faster
  single-day restores
