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

/// Whether `path` looks like catalog material (never a thaw target).
pub fn is_catalog_path(path: &Path) -> bool {
    storage::is_non_blob_prefix(path)
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

        let plan = classify_blobs(&backend, &[path.clone()], 4).await.unwrap();
        assert_eq!(plan.available, vec![path]);
        assert!(plan.all_readable());
        assert_eq!(plan.blocked_count(), 0);
    }

    #[tokio::test]
    async fn classify_local_missing() {
        let datadir = tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(datadir.path()));
        let path = blob_path_for("missing-blob");
        let plan = classify_blobs(&backend, &[path.clone()], 2).await.unwrap();
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
}
