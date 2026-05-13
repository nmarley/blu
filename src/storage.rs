use multihash::Multihash;
use std::path::{Path, PathBuf};

mod local;
mod s3;

use crate::hash::Hash;

// Storage adapter
// types: local, s3, do, azure_blob, gcs

/// The `Backend` trait provides an abstraction over different storage
/// backends.
///
/// It defines a common interface that can be used to interact with various
/// types of storage, such as local file systems, Amazon S3, Google Cloud
/// Storage, etc. This allows code to be written in a storage-agnostic way,
/// where the exact storage backend used is a runtime detail.
///
/// Implementations of `Backend` are responsible for handling the
/// specific details of interacting with the storage backend, such as network
/// communication, error handling, serialization and deserialization of data,
/// etc.
///
/// # Methods
///
/// - `read_data`: Reads data identified by the given hash from the storage
///   backend. It returns a `Result` which, on success, contains the data as a
///   vector of bytes. On failure, it returns an error.
///
/// - `write_data`: Writes data to the storage backend. The path of the data is
///   chosen based on the provided hash. It returns a `Result` which, on success,
///   contains the `PathBuf` where the data is stored. On failure, it returns an
///   error.
///
/// - `exists`: Checks if a blob exists at the given path. Returns true if the
///   blob exists, false otherwise.
///
/// - `delete`: Deletes a blob at the given path. Returns an error if the
///   deletion fails.
trait Backend {
    // TODO: Maybe we want to stream it instead? Make this return a reader?
    //
    // Note: this is only r/w'ing a blob (collection of chunks) at a time, so
    // around 8MiB by default ... maybe streaming doesn't make sense here.
    /// Read the data blob identified by the hash from the storage backend.
    fn read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>>;
    /// Write the data to the storage backend. The path is chosen based on the
    /// hash.
    fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>>;
    /// Check if a blob exists at the given path.
    fn exists(&self, path: &Path) -> Result<bool, Box<dyn std::error::Error>>;
    /// Delete a blob at the given path.
    fn delete(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>>;

    /// Write data to a known path in the backend (not hash-derived).
    ///
    /// Used for index files and other data that must live at a
    /// predictable location rather than a content-addressed path.
    fn write_to_path(&self, path: &Path, data: &[u8]) -> Result<(), Box<dyn std::error::Error>>;

    /// Read data from a known path in the backend (not hash-derived).
    ///
    /// Counterpart to [`write_to_path`]. Used for retrieving index
    /// files and other data stored at predictable locations.
    fn read_from_path(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>>;
}

/// Concrete enum dispatch over supported storage backends.
///
/// Replaces `Box<dyn Backend>` / `&dyn Backend` with a closed set of
/// variants. This enables a future migration to `async fn` methods
/// (native async traits are not object-safe, so `dyn Backend` cannot
/// be used once the methods become async).
pub enum BackendKind {
    /// Local filesystem storage.
    Local(Local),
    /// Amazon S3 storage.
    AmazonS3(AmazonS3),
}

impl BackendKind {
    /// Read the data blob identified by the hash from the storage backend.
    pub fn read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        match self {
            Self::Local(b) => b.read_data(path),
            Self::AmazonS3(b) => b.read_data(path),
        }
    }

    /// Write the data to the storage backend. The path is chosen based on
    /// the hash.
    pub fn write_data(
        &self,
        hash: &Hash,
        data: &[u8],
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        match self {
            Self::Local(b) => b.write_data(hash, data),
            Self::AmazonS3(b) => b.write_data(hash, data),
        }
    }

    /// Check if a blob exists at the given path.
    pub fn exists(&self, path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
        match self {
            Self::Local(b) => b.exists(path),
            Self::AmazonS3(b) => b.exists(path),
        }
    }

    /// Delete a blob at the given path.
    pub fn delete(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            Self::Local(b) => b.delete(path),
            Self::AmazonS3(b) => b.delete(path),
        }
    }

    /// Write data to a known path in the backend (not hash-derived).
    pub fn write_to_path(
        &self,
        path: &Path,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            Self::Local(b) => b.write_to_path(path, data),
            Self::AmazonS3(b) => b.write_to_path(path, data),
        }
    }

    /// Read data from a known path in the backend (not hash-derived).
    pub fn read_from_path(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        match self {
            Self::Local(b) => b.read_from_path(path),
            Self::AmazonS3(b) => b.read_from_path(path),
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
pub fn path_for(hash: &Hash) -> Result<PathBuf, Box<dyn std::error::Error>> {
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
pub fn hash_from_path<P: AsRef<Path>>(path: P) -> Result<Hash, Box<dyn std::error::Error>> {
    let file_name = path.as_ref().file_name().ok_or_else(|| {
        Box::new(std::io::Error::other(
            "Failed to extract file name from path",
        ))
    })?;

    let path_str = file_name
        .to_str()
        .ok_or_else(|| Box::new(std::io::Error::other("Failed to convert file name to str")))?;

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

    #[test]
    fn local_rw_data() {
        let datadir = tempdir().unwrap();
        let storage = BackendKind::Local(Local::new(datadir));

        // Test data
        let data = b"Hello, world!";
        let mh = multihash(data);
        let hash = Hash::from(mh.to_bytes());

        // Write the data
        let pathbuf = storage.write_data(&hash, data).unwrap();

        // Read the data back and verify it
        let read_data = storage.read_data(&pathbuf).unwrap();
        assert_eq!(data.to_vec(), read_data);
    }

    #[test]
    fn local_exists_and_delete() {
        let datadir = tempdir().unwrap();
        let storage = BackendKind::Local(Local::new(&datadir));

        // Test data
        let data = b"Test data for exists/delete";
        let mh = multihash(data);
        let hash = Hash::from(mh.to_bytes());

        // Initially the file should not exist
        let pathbuf = datadir.path().join(path_for(&hash).unwrap());
        assert!(!storage.exists(&pathbuf).unwrap());

        // Write the data
        let written_path = storage.write_data(&hash, data).unwrap();
        assert_eq!(pathbuf, written_path);

        // Now it should exist
        assert!(storage.exists(&pathbuf).unwrap());

        // Delete it
        storage.delete(&pathbuf).unwrap();

        // Now it should not exist
        assert!(!storage.exists(&pathbuf).unwrap());
    }
}

// re-export backends (crate-visible for BackendKind construction)
pub(crate) use local::Local;
pub(crate) use s3::AmazonS3;
