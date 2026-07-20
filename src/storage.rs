use multihash::Multihash;
use std::path::{Path, PathBuf};

mod intelligent_tiering;
mod local;
mod s3;

use crate::error::BluError;
use crate::hash::Hash;

pub use intelligent_tiering::{
    apply_command_hint as intelligent_tiering_apply_hint,
    config_json as intelligent_tiering_config_json, DEFAULT_DEEP_ARCHIVE_DAYS,
    DEFAULT_IT_CONFIG_ID, MAX_ARCHIVE_DAYS, MIN_ARCHIVE_DAYS, MIN_DEEP_ARCHIVE_DAYS,
};

/// Top-level backend directory holding all content-addressed blobs.
///
/// Catalog material lives in sibling directories (`indexes/`, `keys/`),
/// so bucket filters and listing can scope to this prefix alone.
pub const BLOB_PREFIX: &str = "blobs";

/// Object tag key for blu role classification on S3 puts.
pub const TAG_ROLE_KEY: &str = "blu-role";
/// Tag value for content-addressed blob objects (Intelligent-Tiering candidates).
pub const TAG_ROLE_BLOB: &str = "blob";
/// Tag value for catalog material (`indexes/`, `keys/`); stays STANDARD.
pub const TAG_ROLE_CATALOG: &str = "catalog";

/// How quickly S3 should process an archive restore request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestoreTier {
    /// Cheapest; multi-hour for Deep Archive Access.
    #[default]
    Bulk,
    /// Faster standard restore (still hours for Deep Archive Access).
    Standard,
}

/// Parameters for [`BackendKind::restore_object`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreOptions {
    /// Days a temporary Glacier copy stays available (classic Glacier /
    /// Deep Archive storage classes only; ignored for Intelligent-Tiering).
    pub days: u32,
    /// Retrieval tier for the restore job.
    pub tier: RestoreTier,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        Self {
            days: 14,
            tier: RestoreTier::Bulk,
        }
    }
}

/// Whether an object can be read with GET without waiting on a restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectAvailability {
    /// Immediate GET works (hot / IA / Archive Instant / local).
    Available,
    /// In an archive tier; call restore before GET.
    Archived,
    /// Restore job is in progress.
    Restoring,
    /// Temporarily restored (classic Glacier) or mid re-warm; GET works.
    Restored {
        /// Optional expiry hint from the S3 restore header.
        expiry_hint: Option<String>,
    },
}

/// Metadata from a non-GET object probe (HeadObject on S3).
///
/// Prefer this over GET for status and doctor so probes do not count as
/// access that re-warms Intelligent-Tiering objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectStat {
    /// Relative backend path that was probed.
    pub path: PathBuf,
    /// S3 storage class string when known (e.g. `INTELLIGENT_TIERING`).
    pub storage_class: Option<String>,
    /// Intelligent-Tiering archive status when present
    /// (`ARCHIVE_ACCESS` / `DEEP_ARCHIVE_ACCESS`).
    pub archive_status: Option<String>,
    /// Derived availability for GET.
    pub availability: ObjectAvailability,
    /// Raw `x-amz-restore` header value when present.
    pub restore_header: Option<String>,
    /// Object size in bytes when known.
    pub content_length: Option<u64>,
}

/// Summary of a bucket's Intelligent-Tiering configurations.
///
/// Used by doctor to verify the bucket actually archives blobs;
/// blobs upload as `INTELLIGENT_TIERING` but never reach Deep Archive
/// Access without an operator-applied bucket configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItConfigSummary {
    /// Ids of all Intelligent-Tiering configurations on the bucket.
    pub ids: Vec<String>,
    /// True when at least one enabled configuration includes a Deep
    /// Archive Access tiering.
    pub deep_archive_enabled: bool,
}

/// Concrete enum dispatch over supported storage backends.
///
/// All I/O methods are async and driven by the caller's Tokio runtime.
/// Using a concrete enum (rather than `dyn Trait`) because native async
/// fn in traits is not object-safe.
///
/// `Clone` is cheap for all variants and required for spawning
/// concurrent Tokio tasks during mirror/diff operations.
#[derive(Clone)]
pub enum BackendKind {
    /// Local filesystem storage.
    Local(Local),
    /// Amazon S3 storage.
    AmazonS3(AmazonS3),
}

impl BackendKind {
    // TODO: Maybe we want to stream it instead? Make this return a reader?
    //
    // Note: this is only r/w'ing a blob (collection of chunks) at a time, so
    // around 8MiB by default ... maybe streaming doesn't make sense here.

    /// Read the data blob at the given path from the storage backend.
    pub async fn read_data(&self, path: &Path) -> Result<Vec<u8>, BluError> {
        match self {
            Self::Local(b) => b.read_data(path).await,
            Self::AmazonS3(b) => b.read_data(path).await,
        }
    }

    /// Read the byte range `[start, end)` (end exclusive) of the object
    /// at the given path.
    ///
    /// Used by the v3 segmented reader to fetch only the segment prefix
    /// covering a chunk instead of the whole blob. `end` is clamped to
    /// the object length, so a request past EOF returns the available
    /// tail rather than erroring.
    pub async fn read_range(&self, path: &Path, start: u64, end: u64) -> Result<Vec<u8>, BluError> {
        match self {
            Self::Local(b) => b.read_range(path, start, end).await,
            Self::AmazonS3(b) => b.read_range(path, start, end).await,
        }
    }

    /// Write data to a content-addressed path derived from the hash.
    pub async fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, BluError> {
        match self {
            Self::Local(b) => b.write_data(hash, data).await,
            Self::AmazonS3(b) => b.write_data(hash, data).await,
        }
    }

    /// Check if a blob exists at the given path.
    pub async fn exists(&self, path: &Path) -> Result<bool, BluError> {
        match self {
            Self::Local(b) => b.exists(path).await,
            Self::AmazonS3(b) => b.exists(path).await,
        }
    }

    /// Delete a blob at the given path.
    pub async fn delete(&self, path: &Path) -> Result<(), BluError> {
        match self {
            Self::Local(b) => b.delete(path).await,
            Self::AmazonS3(b) => b.delete(path).await,
        }
    }

    /// Write data to a known path in the backend (not hash-derived).
    ///
    /// Used for index files and other data that must live at a
    /// predictable location rather than a content-addressed path.
    pub async fn write_to_path(&self, path: &Path, data: &[u8]) -> Result<(), BluError> {
        match self {
            Self::Local(b) => b.write_to_path(path, data).await,
            Self::AmazonS3(b) => b.write_to_path(path, data).await,
        }
    }

    /// Read data from a known path in the backend (not hash-derived).
    ///
    /// Counterpart to [`write_to_path`]. Used for retrieving index
    /// files and other data stored at predictable locations.
    pub async fn read_from_path(&self, path: &Path) -> Result<Vec<u8>, BluError> {
        match self {
            Self::Local(b) => b.read_from_path(path).await,
            Self::AmazonS3(b) => b.read_from_path(path).await,
        }
    }

    /// List relative paths of content-addressed blob objects.
    ///
    /// Lists only objects under [`BLOB_PREFIX`]; catalog and key
    /// material (`indexes/`, `keys/`) is never walked. Returned paths
    /// match the shape of `BlobIndex::path_index` keys and [`path_for`]
    /// output. Collects the full set into memory; large backends may
    /// be slow.
    pub async fn list_blob_paths(&self) -> Result<Vec<PathBuf>, BluError> {
        match self {
            Self::Local(b) => b.list_blob_paths().await,
            Self::AmazonS3(b) => b.list_blob_paths().await,
        }
    }

    /// Probe object metadata without a GET (HeadObject on S3).
    ///
    /// Used to detect archive tiers and restore progress without
    /// counting as Intelligent-Tiering access.
    pub async fn stat_object(&self, path: &Path) -> Result<ObjectStat, BluError> {
        match self {
            Self::Local(b) => b.stat_object(path).await,
            Self::AmazonS3(b) => b.stat_object(path).await,
        }
    }

    /// Initiate an archive restore for the object at `path`.
    ///
    /// `prior` is an optional earlier probe (e.g. from a thaw
    /// classification); when provided it is trusted and no new HEAD
    /// is issued. Pass `None` to re-probe current state first.
    ///
    /// Idempotent when a restore is already in progress or the object
    /// is already in an active tier. Local backend is a no-op.
    pub async fn restore_object(
        &self,
        path: &Path,
        prior: Option<&ObjectStat>,
        opts: &RestoreOptions,
    ) -> Result<(), BluError> {
        match self {
            Self::Local(b) => b.restore_object(path, prior, opts).await,
            Self::AmazonS3(b) => b.restore_object(path, prior, opts).await,
        }
    }

    /// Summarize the bucket's Intelligent-Tiering configurations.
    ///
    /// Returns `Ok(None)` for backends without bucket configuration
    /// (local). Errors when IAM denies reading bucket configuration;
    /// callers should treat that as warn-only.
    pub async fn intelligent_tiering_summary(&self) -> Result<Option<ItConfigSummary>, BluError> {
        match self {
            Self::Local(b) => b.intelligent_tiering_summary().await,
            Self::AmazonS3(b) => b.intelligent_tiering_summary().await,
        }
    }
}

/// True when a relative backend path lives under [`BLOB_PREFIX`].
pub fn is_blob_path(path: &Path) -> bool {
    path.components()
        .next()
        .is_some_and(|c| c.as_os_str().to_str() == Some(BLOB_PREFIX))
}

/// True when a relative backend path is not a content-addressed blob:
/// catalog material (`indexes/`, `keys/`) or anything outside
/// [`BLOB_PREFIX`].
pub fn is_non_blob_prefix(path: &Path) -> bool {
    !is_blob_path(path)
}

/// Get a path for the encrypted data.
///
/// This is generally the hash of the data, but broken into a dir structure also with the
/// multihash prefix(es) removed from the front, under the top-level `blobs/` directory...
///
/// example, this hash ... :
/// 1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
///
/// ... would be stored in:
/// DATADIR / blobs / d / dd4 / dd4ce / dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
///
pub fn path_for(hash: &Hash) -> Result<PathBuf, BluError> {
    // use multihash lib to properly separate multihash header code and size
    // (do not make assumptions about removing X number of bytes)

    let mh: Multihash<64> = Multihash::from_bytes(&hash.to_bytes())?;
    // dbg!(&mh.code());
    // dbg!(&mh.size());
    // dbg!(&mh.digest());

    let hash_str = hex::encode(mh.digest());
    // dbg!(&hash_str);

    let rel_path = PathBuf::new()
        .join(BLOB_PREFIX)
        .join(&hash_str[0..1])
        .join(&hash_str[0..3])
        .join(&hash_str[0..5])
        .join(&hash_str);

    Ok(rel_path)
}

/// extract the Hash part of the path and return it
pub fn hash_from_path<P: AsRef<Path>>(path: P) -> Result<Hash, BluError> {
    let file_name = path
        .as_ref()
        .file_name()
        .ok_or_else(|| BluError::Internal("failed to extract file name from path".into()))?;

    let path_str = file_name
        .to_str()
        .ok_or_else(|| BluError::Internal("failed to convert file name to str".into()))?;

    Ok(Hash::from(path_str))
}

#[cfg(test)]
mod test {
    use std::path::PathBuf;

    use super::path_for;
    use crate::hash::Hash;

    // macro which tests several different hash algos
    macro_rules! test_path_for {
        ($name:ident, $hash:expr, $path:expr) => {
            #[test]
            fn $name() {
                let hash = Hash::from($hash);
                let path = path_for(&hash).unwrap();
                assert_eq!(path, PathBuf::from($path));
            }
        };
    }

    // DATADIR / blobs / d / dd4 / dd4ce / dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
    test_path_for!(
        path_for_sha2_512,
        "1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6",
        "blobs/d/dd4/dd4ce/dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6"
    );
    test_path_for!(
        path_for_sha2_256,
        "12209b2f4374822ae5b8a14e89f69bdcc1b570948e201f318c763ee1c31d2fb02f3d",
        "blobs/9/9b2/9b2f4/9b2f4374822ae5b8a14e89f69bdcc1b570948e201f318c763ee1c31d2fb02f3d"
    );
    test_path_for!(
        path_for_sha3_256,
        "16202a62db58c655ef1484f5c5d8bbd8eb9b75261a149db76b9e0177831325f5030e",
        "blobs/2/2a6/2a62d/2a62db58c655ef1484f5c5d8bbd8eb9b75261a149db76b9e0177831325f5030e"
    );
    test_path_for!(
        path_for_blake2b_256,
        "a0e4022064982f9ad98dc4845638d6ed1abc2ef2f76d90eecc9091e4802e73734b96ec36",
        "blobs/6/649/64982/64982f9ad98dc4845638d6ed1abc2ef2f76d90eecc9091e4802e73734b96ec36"
    );

    use std::path::Path;
    use tempfile::tempdir;

    use super::BackendKind;
    use super::Local;
    use crate::hash::multihash;

    #[tokio::test]
    async fn local_rw_data() {
        let datadir = tempdir().unwrap();
        let storage = BackendKind::Local(Local::new(datadir));

        let data = b"Hello, world!";
        let mh = multihash(data);
        let hash = Hash::from(mh.to_bytes());

        // write_data returns a relative content-addressed path
        let rel_path = storage.write_data(&hash, data).await.unwrap();
        assert_eq!(rel_path, path_for(&hash).unwrap());

        // read_data accepts the same relative path
        let read_data = storage.read_data(&rel_path).await.unwrap();
        assert_eq!(data.to_vec(), read_data);
    }

    #[tokio::test]
    async fn local_exists_and_delete() {
        let datadir = tempdir().unwrap();
        let storage = BackendKind::Local(Local::new(&datadir));

        let data = b"Test data for exists/delete";
        let mh = multihash(data);
        let hash = Hash::from(mh.to_bytes());

        // All methods use relative content-addressed paths;
        // the backend prepends datadir internally.
        let rel_path = path_for(&hash).unwrap();

        assert!(!storage.exists(&rel_path).await.unwrap());

        let written_path = storage.write_data(&hash, data).await.unwrap();
        assert_eq!(rel_path, written_path);

        assert!(storage.exists(&rel_path).await.unwrap());

        storage.delete(&rel_path).await.unwrap();

        assert!(!storage.exists(&rel_path).await.unwrap());
    }

    #[tokio::test]
    async fn local_read_range_returns_exact_window() {
        let datadir = tempdir().unwrap();
        let storage = BackendKind::Local(Local::new(datadir));

        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let mh = multihash(&data);
        let hash = Hash::from(mh.to_bytes());
        let rel_path = storage.write_data(&hash, &data).await.unwrap();

        // Interior window [100, 200) is exactly 100 bytes and matches.
        let window = storage.read_range(&rel_path, 100, 200).await.unwrap();
        assert_eq!(window, &data[100..200]);

        // Leading window [0, 16).
        let head = storage.read_range(&rel_path, 0, 16).await.unwrap();
        assert_eq!(head, &data[0..16]);
    }

    #[tokio::test]
    async fn local_read_range_clamps_at_eof() {
        let datadir = tempdir().unwrap();
        let storage = BackendKind::Local(Local::new(datadir));

        let data = b"short blob".to_vec();
        let mh = multihash(&data);
        let hash = Hash::from(mh.to_bytes());
        let rel_path = storage.write_data(&hash, &data).await.unwrap();

        // End past EOF returns the available tail, not an error.
        let tail = storage.read_range(&rel_path, 4, 10_000).await.unwrap();
        assert_eq!(tail, &data[4..]);

        // Start past EOF returns empty.
        let empty = storage.read_range(&rel_path, 10_000, 20_000).await.unwrap();
        assert!(empty.is_empty());

        // Empty window (end <= start) returns empty.
        let zero = storage.read_range(&rel_path, 5, 5).await.unwrap();
        assert!(zero.is_empty());
    }

    #[tokio::test]
    async fn local_list_blob_paths_scoped_to_blob_prefix() {
        use super::is_non_blob_prefix;
        use std::fs;

        assert!(is_non_blob_prefix(Path::new("indexes/index.dat")));
        assert!(is_non_blob_prefix(Path::new("keys/kek.toml")));
        assert!(is_non_blob_prefix(Path::new("d/dd4/legacy-shard")));
        assert!(!is_non_blob_prefix(Path::new(
            "blobs/d/dd4/dd4ce/dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6"
        )));

        let datadir = tempdir().unwrap();
        let storage = BackendKind::Local(Local::new(datadir.path()));

        let data_a = b"blob-a";
        let data_b = b"blob-b";
        let path_a = storage
            .write_data(&Hash::from(multihash(data_a).to_bytes()), data_a)
            .await
            .unwrap();
        let path_b = storage
            .write_data(&Hash::from(multihash(data_b).to_bytes()), data_b)
            .await
            .unwrap();

        // Catalog / key objects must not appear in the blob list.
        storage
            .write_to_path(Path::new("indexes/index.dat"), b"plain-index")
            .await
            .unwrap();
        storage
            .write_to_path(Path::new("keys/kek.toml"), b"kek-meta")
            .await
            .unwrap();
        // Stray top-level dirs and empty shard dirs under blobs/ are
        // not files, so neither produces entries.
        fs::create_dir_all(datadir.path().join("z/zzz/zzzzz")).unwrap();
        fs::create_dir_all(datadir.path().join("blobs/z/zzz/zzzzz")).unwrap();

        let listed = storage.list_blob_paths().await.unwrap();
        assert_eq!(listed.len(), 2, "listed={listed:?}");
        assert!(listed.contains(&path_a));
        assert!(listed.contains(&path_b));
        assert!(listed.iter().all(|p| !is_non_blob_prefix(p)));
    }

    #[tokio::test]
    async fn local_list_blob_paths_empty_datadir() {
        let datadir = tempdir().unwrap();
        let storage = BackendKind::Local(Local::new(datadir.path().join("missing")));
        let listed = storage.list_blob_paths().await.unwrap();
        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn local_stat_object_available() {
        use super::{ObjectAvailability, RestoreOptions};

        let datadir = tempdir().unwrap();
        let storage = BackendKind::Local(Local::new(datadir.path()));
        let data = b"stat-me";
        let path = storage
            .write_data(&Hash::from(multihash(data).to_bytes()), data)
            .await
            .unwrap();

        let stat = storage.stat_object(&path).await.unwrap();
        assert_eq!(stat.availability, ObjectAvailability::Available);
        assert_eq!(stat.content_length, Some(data.len() as u64));
        assert!(stat.storage_class.is_none());

        storage
            .restore_object(&path, None, &RestoreOptions::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn local_stat_missing_is_not_found() {
        let datadir = tempdir().unwrap();
        let storage = BackendKind::Local(Local::new(datadir.path()));
        let err = storage
            .stat_object(Path::new("no/such/blob"))
            .await
            .unwrap_err();
        match err {
            crate::error::BluError::StorageFileNotFound { path } => {
                assert_eq!(path, PathBuf::from("no/such/blob"));
            }
            other => panic!("expected StorageFileNotFound, got {other:?}"),
        }
    }
}

// re-export backends (crate-visible for BackendKind construction)
pub(crate) use local::Local;
pub(crate) use s3::AmazonS3;
