use multihash::Multihash;
use std::path::{Path, PathBuf};

mod local;
mod s3;

use crate::error::BluError;
use crate::hash::Hash;

// Storage adapter
// types: local, s3, do, azure_blob, gcs

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
}

/// Get a path for the encrypted data.
///
/// This is generally the hash of the data, but broken into a dir structure also with the
/// multihash prefix(es) removed from the front...
///
/// example, this hash ... :
/// 1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
///
/// ... would be stored in:
/// DATADIR / d / dd4 / dd4ce / dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
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

    // DATADIR / d / dd4 / dd4ce38e / dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
    test_path_for!(
        path_for_sha2_512,
        "1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6",
        "d/dd4/dd4ce/dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6"
    );
    test_path_for!(
        path_for_sha2_256,
        "12209b2f4374822ae5b8a14e89f69bdcc1b570948e201f318c763ee1c31d2fb02f3d",
        "9/9b2/9b2f4/9b2f4374822ae5b8a14e89f69bdcc1b570948e201f318c763ee1c31d2fb02f3d"
    );
    test_path_for!(
        path_for_sha3_256,
        "16202a62db58c655ef1484f5c5d8bbd8eb9b75261a149db76b9e0177831325f5030e",
        "2/2a6/2a62d/2a62db58c655ef1484f5c5d8bbd8eb9b75261a149db76b9e0177831325f5030e"
    );
    test_path_for!(
        path_for_blake2b_256,
        "a0e4022064982f9ad98dc4845638d6ed1abc2ef2f76d90eecc9091e4802e73734b96ec36",
        "6/649/64982/64982f9ad98dc4845638d6ed1abc2ef2f76d90eecc9091e4802e73734b96ec36"
    );

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
}

// re-export backends (crate-visible for BackendKind construction)
pub(crate) use local::Local;
pub(crate) use s3::AmazonS3;
