//! Plan which catalog files map to which content-addressed blobs, and
//! which of those blobs need archive restore (thaw) before GET.
//!
//! Selection and blob-set planning are pure index work. Classification
//! uses [`BackendKind::stat_object`] (HeadObject on S3) so probes do
//! not count as Intelligent-Tiering access.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use glob::Pattern;
use tokio::sync::Semaphore;

use crate::blob::BlobIndex;
use crate::block::PlainIndex;
use crate::error::BluError;
use crate::hash::Hash;
use crate::storage::{self, BackendKind, ObjectAvailability, ObjectStat, RestoreOptions};

/// How to select files from the plain index for restore or thaw.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// Select every file in the catalog.
    pub all: bool,
    /// Substring / prefix matches against the file content hash hex.
    pub hash_prefixes: Vec<String>,
    /// Glob matched against any path recorded for the file.
    pub path_glob: Option<String>,
}

impl Selection {
    /// True when no selection criteria were provided.
    pub fn is_empty(&self) -> bool {
        !self.all && self.hash_prefixes.is_empty() && self.path_glob.is_none()
    }
}

/// Unique content-addressed blob paths required by a set of catalog files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlobSet {
    /// Selected file content hashes (catalog order is not significant).
    pub file_hashes: Vec<Hash>,
    /// Unique blob backend paths, sorted for stable output.
    pub blob_paths: Vec<PathBuf>,
}

/// One blob's cold-storage classification from a non-GET probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdBlob {
    /// Relative backend path of the blob.
    pub path: PathBuf,
    /// Full stat result from the backend.
    pub stat: ObjectStat,
}

/// Partition of a blob set by cold-storage availability.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColdPlan {
    /// Immediate GET works.
    pub available: Vec<PathBuf>,
    /// Archive tier; needs RestoreObject.
    pub archived: Vec<ColdBlob>,
    /// Restore already in progress.
    pub restoring: Vec<ColdBlob>,
    /// Temporarily restored / mid re-warm; GET works.
    pub restored: Vec<ColdBlob>,
    /// Stat failed with not-found (or missing on local).
    pub missing: Vec<PathBuf>,
    /// Stat failed for other reasons (path + error string).
    pub errors: Vec<(PathBuf, String)>,
}

impl ColdPlan {
    /// Blobs that still block a successful GET (archived or restoring).
    pub fn needs_thaw(&self) -> impl Iterator<Item = &ColdBlob> {
        self.archived.iter().chain(self.restoring.iter())
    }

    /// Number of blobs that still need a restore job or wait.
    pub fn blocked_count(&self) -> usize {
        self.archived.len() + self.restoring.len()
    }

    /// True when every present blob can be read with GET now.
    pub fn all_readable(&self) -> bool {
        self.archived.is_empty()
            && self.restoring.is_empty()
            && self.missing.is_empty()
            && self.errors.is_empty()
    }
}

/// Select file content hashes from `plain` matching `selection`.
///
/// Invalid globs are escaped and treated as literals (same as restore).
pub fn match_files(plain: &PlainIndex, selection: &Selection) -> Result<Vec<Hash>, BluError> {
    if selection.is_empty() {
        return Err(BluError::Internal(
            "Must specify --file-hashes, --path, or --all".into(),
        ));
    }

    let path_pattern = match selection.path_glob.as_ref() {
        Some(p) => match Pattern::new(p) {
            Ok(pat) => Some(pat),
            Err(_) => Some(Pattern::new(&glob::Pattern::escape(p)).map_err(|e| {
                BluError::Internal(format!("failed to escape glob pattern '{}': {}", p, e))
            })?),
        },
        None => None,
    };

    let mut out: Vec<Hash> = Vec::new();
    for (hash, fileref) in plain.files_map_ref() {
        let mut selected = selection.all;

        if !selected && !selection.hash_prefixes.is_empty() {
            let hash_str = hash.to_string();
            selected = selection
                .hash_prefixes
                .iter()
                .any(|prefix| hash_str.contains(prefix.as_str()));
        }

        if !selected {
            if let Some(ref pattern) = path_pattern {
                selected = fileref.paths.iter().any(|path| pattern.matches_path(path));
            }
        }

        if selected {
            out.push(hash.clone());
        }
    }

    out.sort_by_key(|a| a.to_string());
    Ok(out)
}

/// Collect unique content-addressed blob paths for the given file hashes.
///
/// Chunks missing from the blob index are skipped (caller may still fail
/// later if a file cannot be fully restored).
pub fn blob_paths_for_files(
    plain: &PlainIndex,
    blob_index: &BlobIndex,
    file_hashes: &[Hash],
) -> Result<Vec<PathBuf>, BluError> {
    let mut by_blob_hash: HashMap<Hash, PathBuf> = HashMap::new();

    for file_hash in file_hashes {
        let Some(fileref) = plain.get_fileref_ref(file_hash) else {
            continue;
        };
        for chunkmeta in &fileref.chunkmetas {
            if !blob_index.has_chunk(&chunkmeta.hash) {
                continue;
            }
            let location = blob_index.get_block_location_ref(&chunkmeta.hash)?;
            let blob_hash = storage::hash_from_path(location.blob_path())?;
            by_blob_hash
                .entry(blob_hash)
                .or_insert_with(|| location.blob_path().clone());
        }
    }

    let mut paths: Vec<PathBuf> = by_blob_hash.into_values().collect();
    paths.sort();
    Ok(paths)
}

/// Build a [`BlobSet`] from catalog selection criteria.
pub fn plan_blob_set(
    plain: &PlainIndex,
    blob_index: &BlobIndex,
    selection: &Selection,
) -> Result<BlobSet, BluError> {
    let file_hashes = match_files(plain, selection)?;
    let blob_paths = blob_paths_for_files(plain, blob_index, &file_hashes)?;
    Ok(BlobSet {
        file_hashes,
        blob_paths,
    })
}

/// Classify blob paths by cold-storage availability via backend stat.
///
/// Concurrent HeadObject probes; does not initiate restore.
pub async fn classify_blobs(
    backend: &BackendKind,
    blob_paths: &[PathBuf],
    concurrency: usize,
) -> Result<ColdPlan, BluError> {
    if blob_paths.is_empty() {
        return Ok(ColdPlan::default());
    }

    let concurrency = concurrency.max(1);
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::with_capacity(blob_paths.len());

    for path in blob_paths {
        let backend = backend.clone();
        let path = path.clone();
        let sem = Arc::clone(&sem);
        handles.push(tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|e| BluError::Internal(format!("semaphore closed: {e}")))?;
            match backend.stat_object(&path).await {
                Ok(stat) => Ok::<_, BluError>((path, Ok(stat))),
                Err(BluError::StorageFileNotFound { .. }) => Ok((path, Err(None))),
                Err(e) => Ok((path, Err(Some(e.to_string())))),
            }
        }));
    }

    let mut plan = ColdPlan::default();
    for handle in handles {
        let (path, result) = handle
            .await
            .map_err(|e| BluError::Internal(format!("classify join: {e}")))??;
        match result {
            Ok(stat) => match &stat.availability {
                ObjectAvailability::Available => plan.available.push(path),
                ObjectAvailability::Archived => plan.archived.push(ColdBlob { path, stat }),
                ObjectAvailability::Restoring => plan.restoring.push(ColdBlob { path, stat }),
                ObjectAvailability::Restored { .. } => plan.restored.push(ColdBlob { path, stat }),
            },
            Err(None) => plan.missing.push(path),
            Err(Some(msg)) => plan.errors.push((path, msg)),
        }
    }

    plan.available.sort();
    plan.missing.sort();
    plan.archived.sort_by(|a, b| a.path.cmp(&b.path));
    plan.restoring.sort_by(|a, b| a.path.cmp(&b.path));
    plan.restored.sort_by(|a, b| a.path.cmp(&b.path));
    plan.errors.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(plan)
}

/// Plan blobs for a selection and classify which need thaw.
pub async fn plan_cold(
    plain: &PlainIndex,
    blob_index: &BlobIndex,
    selection: &Selection,
    backend: &BackendKind,
    concurrency: usize,
) -> Result<(BlobSet, ColdPlan), BluError> {
    let set = plan_blob_set(plain, blob_index, selection)?;
    let cold = classify_blobs(backend, &set.blob_paths, concurrency).await?;
    Ok((set, cold))
}

/// All content-addressed blob paths known to the blob index.
pub fn all_indexed_blob_paths(blob_index: &BlobIndex) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = blob_index.path_index.keys().cloned().collect();
    paths.sort();
    paths
}

/// Evenly sample up to `cap` paths from a sorted slice.
///
/// Deterministic stride sampling (`i * len / cap`) so repeated doctor
/// runs cover the whole keyspace instead of always probing the same
/// shard region. Returns the input unchanged when it fits under `cap`.
pub fn sample_evenly(paths: &[PathBuf], cap: usize) -> Vec<PathBuf> {
    if cap == 0 {
        return Vec::new();
    }
    if paths.len() <= cap {
        return paths.to_vec();
    }
    (0..cap)
        .map(|i| paths[i * paths.len() / cap].clone())
        .collect()
}

/// Result of initiating RestoreObject on archived blobs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThawInitResult {
    /// Paths for which RestoreObject was requested successfully.
    pub initiated: Vec<PathBuf>,
    /// Paths already restoring (no new request needed).
    pub already_restoring: Vec<PathBuf>,
    /// Paths that failed to initiate (path + error).
    pub failed: Vec<(PathBuf, String)>,
}

/// Initiate archive restores for archived blobs in `plan`.
///
/// Blobs already restoring are counted but not re-requested.
/// Available and restored blobs are ignored.
pub async fn initiate_thaw(
    backend: &BackendKind,
    plan: &ColdPlan,
    opts: &RestoreOptions,
    concurrency: usize,
) -> Result<ThawInitResult, BluError> {
    let mut result = ThawInitResult {
        already_restoring: plan.restoring.iter().map(|b| b.path.clone()).collect(),
        ..ThawInitResult::default()
    };
    result.already_restoring.sort();

    if plan.archived.is_empty() {
        return Ok(result);
    }

    let concurrency = concurrency.max(1);
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::with_capacity(plan.archived.len());

    for cold in &plan.archived {
        let backend = backend.clone();
        let path = cold.path.clone();
        // Hand the classification stat to restore_object so the thaw
        // does not issue a second HEAD per blob.
        let stat = cold.stat.clone();
        let opts = *opts;
        let sem = Arc::clone(&sem);
        handles.push(tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|e| BluError::Internal(format!("semaphore closed: {e}")))?;
            match backend.restore_object(&path, Some(&stat), &opts).await {
                Ok(()) => Ok::<_, BluError>((path, None)),
                Err(e) => Ok((path, Some(e.to_string()))),
            }
        }));
    }

    for handle in handles {
        let (path, err) = handle
            .await
            .map_err(|e| BluError::Internal(format!("initiate_thaw join: {e}")))??;
        match err {
            None => result.initiated.push(path),
            Some(msg) => result.failed.push((path, msg)),
        }
    }

    result.initiated.sort();
    result.failed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}

/// Consecutive polls containing stat errors before `wait_until_readable`
/// gives up (guards against infinite error loops when no timeout is set).
const MAX_CONSECUTIVE_ERROR_POLLS: u32 = 5;

/// Initial poll interval for cold waits.
pub const WAIT_POLL_INITIAL_SECS: u64 = 30;

/// Upper bound for the cold wait poll interval.
pub const WAIT_POLL_MAX_SECS: u64 = 300;

/// Exponential backoff between cold wait classification passes.
///
/// Deep Archive restores run for hours, so a fixed short interval
/// wastes HEAD requests and floods logs. The interval doubles from
/// `initial` up to `max`; both are clamped to a sane nonzero range.
#[derive(Debug, Clone)]
pub struct PollBackoff {
    next: std::time::Duration,
    max: std::time::Duration,
}

impl PollBackoff {
    /// Backoff starting at `initial` and doubling up to `max`.
    pub fn new(initial: std::time::Duration, max: std::time::Duration) -> Self {
        let floor = std::time::Duration::from_millis(1);
        let max = max.max(floor);
        Self {
            next: initial.clamp(floor, max),
            max,
        }
    }

    /// Current interval; the following call returns at most double,
    /// capped at `max`.
    pub fn next_interval(&mut self) -> std::time::Duration {
        let current = self.next;
        self.next = self.next.saturating_mul(2).min(self.max);
        current
    }
}

/// Re-classify `blob_paths` until none are blocked, or `timeout` elapses.
///
/// Returns the final cold plan. When `timeout` is `None`, polls until
/// readable or the process is interrupted. Missing blobs are terminal
/// (waiting cannot bring them back), and a run of consecutive polls
/// with stat errors bails out instead of looping forever. `backoff`
/// spaces out classification passes (see [`PollBackoff`]).
pub async fn wait_until_readable(
    backend: &BackendKind,
    blob_paths: &[PathBuf],
    concurrency: usize,
    mut backoff: PollBackoff,
    timeout: Option<std::time::Duration>,
) -> Result<ColdPlan, BluError> {
    let started = std::time::Instant::now();
    let mut error_polls = 0u32;
    loop {
        let plan = classify_blobs(backend, blob_paths, concurrency).await?;

        if !plan.missing.is_empty() {
            return Err(BluError::StorageError(format!(
                "{} blob(s) missing from backend (e.g. {}); cannot wait for \
                 objects that do not exist",
                plan.missing.len(),
                plan.missing[0].display(),
            )));
        }

        if plan.blocked_count() == 0 && plan.errors.is_empty() {
            return Ok(plan);
        }

        if plan.errors.is_empty() {
            error_polls = 0;
        } else {
            error_polls += 1;
            if error_polls >= MAX_CONSECUTIVE_ERROR_POLLS {
                return Err(BluError::StorageError(format!(
                    "{} consecutive poll(s) with stat errors ({} error(s) in last \
                     poll, e.g. {}); giving up",
                    error_polls,
                    plan.errors.len(),
                    plan.errors[0].1,
                )));
            }
        }

        if let Some(limit) = timeout {
            if started.elapsed() >= limit {
                return Err(BluError::StorageError(format!(
                    "timed out waiting for {} archived blob(s) to become readable \
                     ({} still restoring)",
                    plan.archived.len(),
                    plan.restoring.len(),
                )));
            }
        }
        tokio::time::sleep(backoff.next_interval()).await;
    }
}

/// True when availability blocks an immediate GET.
pub fn availability_blocks_get(availability: &ObjectAvailability) -> bool {
    matches!(
        availability,
        ObjectAvailability::Archived | ObjectAvailability::Restoring
    )
}

/// Default restore options for thaw initiation (Bulk, 14 days).
pub fn default_restore_options() -> RestoreOptions {
    RestoreOptions::default()
}

/// Default cold wait backoff: 30s initial, doubling to a 5min cap.
pub fn default_poll_backoff() -> PollBackoff {
    PollBackoff::new(
        std::time::Duration::from_secs(WAIT_POLL_INITIAL_SECS),
        std::time::Duration::from_secs(WAIT_POLL_MAX_SECS),
    )
}

/// Whether `path` looks like catalog material (never a thaw target).
pub fn is_catalog_path(path: &Path) -> bool {
    storage::is_non_blob_prefix(path)
}

/// Human-readable summary of a cold plan for CLI output.
pub fn format_cold_summary(plan: &ColdPlan) -> String {
    format!(
        "available={} restored={} restoring={} archived={} missing={} errors={}",
        plan.available.len(),
        plan.restored.len(),
        plan.restoring.len(),
        plan.archived.len(),
        plan.missing.len(),
        plan.errors.len(),
    )
}

/// Build a fail-fast error when restore cannot proceed due to archive tiers.
pub fn blocked_restore_error(plan: &ColdPlan) -> BluError {
    BluError::StorageError(format!(
        "{} blob(s) are not readable yet ({}); run `blu thaw` with the same \
         selection (or `blu restore ... --thaw`), then retry after restore \
         completes (Deep Archive Access is typically ~12h with Standard, longer \
         with Bulk)",
        plan.blocked_count(),
        format_cold_summary(plan),
    ))
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::blob::BlobBlockLocation;
    use crate::block::{ChunkMeta, FileRef, PlainIndex};
    use crate::hash::multihash;
    use crate::io::Position;
    use crate::storage::Local;
    use tempfile::tempdir;

    fn hash_of(label: &str) -> Hash {
        Hash::from(multihash(label.as_bytes()).to_bytes())
    }

    fn blob_path_for(label: &str) -> PathBuf {
        storage::path_for(&hash_of(label)).unwrap()
    }

    fn plain_with_files(entries: &[(&str, &str, &[&str])]) -> PlainIndex {
        // (file_label, chunk_label, paths)
        let mut plain = PlainIndex::new_empty();
        for (file_label, chunk_label, paths) in entries {
            let file_hash = hash_of(file_label);
            let chunk_hash = hash_of(chunk_label);
            let mut fileref = FileRef::new(vec![ChunkMeta {
                hash: chunk_hash,
                size: 4,
            }]);
            for p in *paths {
                fileref.paths.insert(PathBuf::from(p));
            }
            plain.files.insert(file_hash, fileref);
        }
        plain
    }

    fn blob_index_for(chunks_to_blobs: &[(&str, &str)]) -> BlobIndex {
        let mut idx = BlobIndex::new();
        for (chunk_label, blob_label) in chunks_to_blobs {
            let chunk_hash = hash_of(chunk_label);
            let path = blob_path_for(blob_label);
            let loc = BlobBlockLocation::new(path, Position { offset: 0, size: 4 });
            idx.add_chunk_location(&chunk_hash, &loc);
        }
        idx
    }

    #[test]
    fn match_files_requires_criteria() {
        let plain = PlainIndex::new_empty();
        let err = match_files(&plain, &Selection::default()).unwrap_err();
        assert!(err.to_string().contains("Must specify"));
    }

    #[test]
    fn match_files_by_path_glob() {
        let plain = plain_with_files(&[
            ("file-a", "chunk-a", &["photos/2024/a.jpg"]),
            ("file-b", "chunk-b", &["docs/readme.txt"]),
        ]);
        let sel = Selection {
            path_glob: Some("photos/**".into()),
            ..Selection::default()
        };
        let matched = match_files(&plain, &sel).unwrap();
        assert_eq!(matched, vec![hash_of("file-a")]);
    }

    #[test]
    fn match_files_by_hash_prefix() {
        let plain = plain_with_files(&[("file-a", "chunk-a", &["a.txt"])]);
        let full = hash_of("file-a").to_string();
        let prefix = full[..12].to_string();
        let sel = Selection {
            hash_prefixes: vec![prefix],
            ..Selection::default()
        };
        let matched = match_files(&plain, &sel).unwrap();
        assert_eq!(matched, vec![hash_of("file-a")]);
    }

    #[test]
    fn match_files_all() {
        let plain = plain_with_files(&[
            ("file-a", "chunk-a", &["a.txt"]),
            ("file-b", "chunk-b", &["b.txt"]),
        ]);
        let sel = Selection {
            all: true,
            ..Selection::default()
        };
        let matched = match_files(&plain, &sel).unwrap();
        assert_eq!(matched.len(), 2);
    }

    #[test]
    fn blob_paths_dedup_shared_blob() {
        // Two files, two chunks, same blob object.
        let plain = plain_with_files(&[
            ("file-a", "chunk-a", &["a.txt"]),
            ("file-b", "chunk-b", &["b.txt"]),
        ]);
        let blob_index = blob_index_for(&[("chunk-a", "shared"), ("chunk-b", "shared")]);
        let files = vec![hash_of("file-a"), hash_of("file-b")];
        let paths = blob_paths_for_files(&plain, &blob_index, &files).unwrap();
        assert_eq!(paths, vec![blob_path_for("shared")]);
    }

    #[test]
    fn blob_paths_multiple_blobs() {
        let plain = plain_with_files(&[
            ("file-a", "chunk-a", &["a.txt"]),
            ("file-b", "chunk-b", &["b.txt"]),
        ]);
        let blob_index = blob_index_for(&[("chunk-a", "blob-a"), ("chunk-b", "blob-b")]);
        let files = vec![hash_of("file-a"), hash_of("file-b")];
        let paths = blob_paths_for_files(&plain, &blob_index, &files).unwrap();
        let mut expected = vec![blob_path_for("blob-a"), blob_path_for("blob-b")];
        expected.sort();
        assert_eq!(paths, expected);
    }

    #[test]
    fn plan_blob_set_end_to_end() {
        let plain = plain_with_files(&[("file-a", "chunk-a", &["photos/x.jpg"])]);
        let blob_index = blob_index_for(&[("chunk-a", "blob-a")]);
        let set = plan_blob_set(
            &plain,
            &blob_index,
            &Selection {
                path_glob: Some("photos/*".into()),
                ..Selection::default()
            },
        )
        .unwrap();
        assert_eq!(set.file_hashes, vec![hash_of("file-a")]);
        assert_eq!(set.blob_paths, vec![blob_path_for("blob-a")]);
    }

    #[tokio::test]
    async fn classify_local_all_available() {
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(datadir.path()));
        let path = blob_path_for("local-blob");
        backend
            .write_data(&hash_of("local-blob"), b"payload")
            .await
            .unwrap();

        let plan = classify_blobs(&backend, std::slice::from_ref(&path), 4)
            .await
            .unwrap();
        assert_eq!(plan.available, vec![path]);
        assert!(plan.all_readable());
        assert_eq!(plan.blocked_count(), 0);
    }

    #[tokio::test]
    async fn classify_local_missing() {
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(datadir.path()));
        let path = blob_path_for("missing-blob");
        let plan = classify_blobs(&backend, std::slice::from_ref(&path), 2)
            .await
            .unwrap();
        assert_eq!(plan.missing, vec![path]);
        assert!(!plan.all_readable());
    }

    #[test]
    fn availability_blocks_get_matrix() {
        assert!(!availability_blocks_get(&ObjectAvailability::Available));
        assert!(availability_blocks_get(&ObjectAvailability::Archived));
        assert!(availability_blocks_get(&ObjectAvailability::Restoring));
        assert!(!availability_blocks_get(&ObjectAvailability::Restored {
            expiry_hint: None
        }));
    }

    #[test]
    fn catalog_paths_not_thaw_targets() {
        assert!(is_catalog_path(Path::new("indexes/index.dat")));
        assert!(is_catalog_path(Path::new("keys/kek.toml")));
        assert!(!is_catalog_path(&blob_path_for("x")));
    }

    #[tokio::test]
    async fn initiate_thaw_local_noop() {
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(datadir.path()));
        let path = blob_path_for("local-blob");
        backend
            .write_data(&hash_of("local-blob"), b"payload")
            .await
            .unwrap();
        let plan = classify_blobs(&backend, &[path], 2).await.unwrap();
        let init = initiate_thaw(&backend, &plan, &RestoreOptions::default(), 2)
            .await
            .unwrap();
        assert!(init.initiated.is_empty());
        assert!(init.failed.is_empty());
        assert!(plan.all_readable());
    }

    #[tokio::test]
    async fn wait_until_readable_local_available_immediately() {
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(datadir.path()));
        let path = blob_path_for("local-blob");
        backend
            .write_data(&hash_of("local-blob"), b"payload")
            .await
            .unwrap();

        let plan = wait_until_readable(
            &backend,
            std::slice::from_ref(&path),
            2,
            PollBackoff::new(
                std::time::Duration::from_millis(10),
                std::time::Duration::from_millis(40),
            ),
            None,
        )
        .await
        .unwrap();
        assert!(plan.all_readable());
    }

    #[tokio::test]
    async fn wait_until_readable_missing_is_terminal() {
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(datadir.path()));
        let path = blob_path_for("missing-blob");

        // No timeout set: a missing blob must fail fast, not loop.
        let err = wait_until_readable(
            &backend,
            std::slice::from_ref(&path),
            2,
            PollBackoff::new(
                std::time::Duration::from_millis(10),
                std::time::Duration::from_millis(40),
            ),
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("missing"), "{err}");
    }

    #[test]
    fn sample_evenly_returns_all_under_cap() {
        let paths = vec![PathBuf::from("a"), PathBuf::from("b")];
        assert_eq!(sample_evenly(&paths, 4), paths);
        assert_eq!(sample_evenly(&paths, 2), paths);
        assert!(sample_evenly(&paths, 0).is_empty());
    }

    #[test]
    fn sample_evenly_covers_keyspace() {
        let paths: Vec<PathBuf> = (0..1000)
            .map(|i| PathBuf::from(format!("p{:04}", i)))
            .collect();
        let sampled = sample_evenly(&paths, 10);
        assert_eq!(sampled.len(), 10);
        // Stride i * len / cap hits first, last-ish, and evenly between.
        assert_eq!(sampled[0], PathBuf::from("p0000"));
        assert_eq!(sampled[9], PathBuf::from("p0900"));
        // Deterministic across calls.
        assert_eq!(sample_evenly(&paths, 10), sampled);
    }

    #[test]
    fn poll_backoff_doubles_up_to_cap() {
        use std::time::Duration;
        let mut backoff = PollBackoff::new(Duration::from_secs(30), Duration::from_secs(300));
        let seq: Vec<u64> = (0..7).map(|_| backoff.next_interval().as_secs()).collect();
        assert_eq!(seq, vec![30, 60, 120, 240, 300, 300, 300]);
    }

    #[test]
    fn poll_backoff_clamps_degenerate_ranges() {
        use std::time::Duration;
        // Initial above the cap starts at the cap.
        let mut backoff = PollBackoff::new(Duration::from_secs(600), Duration::from_secs(300));
        assert_eq!(backoff.next_interval(), Duration::from_secs(300));
        // Zero initial is floored to a nonzero yield, never a spin loop.
        let mut backoff = PollBackoff::new(Duration::ZERO, Duration::from_millis(4));
        assert_eq!(backoff.next_interval(), Duration::from_millis(1));
        assert_eq!(backoff.next_interval(), Duration::from_millis(2));
        assert_eq!(backoff.next_interval(), Duration::from_millis(4));
        assert_eq!(backoff.next_interval(), Duration::from_millis(4));
    }

    #[test]
    fn format_cold_summary_counts() {
        let plan = ColdPlan {
            available: vec![PathBuf::from("a")],
            archived: vec![],
            restoring: vec![],
            restored: vec![],
            missing: vec![PathBuf::from("m")],
            errors: vec![],
        };
        assert!(format_cold_summary(&plan).contains("available=1"));
        assert!(format_cold_summary(&plan).contains("missing=1"));
    }
}
