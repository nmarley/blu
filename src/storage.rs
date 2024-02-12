use async_trait::async_trait;
use multihash::Multihash;
use std::path::{Path, PathBuf};

mod local;
mod s3;

use crate::hash::Hash;

// TODO : Decouple filesystem semantics from storage backend

// TODO: probably rename StorageBackend to Backend to prevent stuttering ...
// crate::storage::StorageBackend
// crate::storage::Backend

// Storage adapter
// types: local, s3, do, azure_blob, gcs

/// The `StorageBackend` trait provides an abstraction over different storage
/// backends.
///
/// It defines a common interface that can be used to interact with various
/// types of storage, such as local file systems, Amazon S3, Google Cloud
/// Storage, etc. This allows code to be written in a storage-agnostic way,
/// where the exact storage backend used is a runtime detail.
///
/// Implementations of `StorageBackend` are responsible for handling the
/// specific details of interacting with the storage backend, such as network
/// communication, error handling, serialization and deserialization of data,
/// etc.
///
/// # Methods
///
/// - `read_data`: Reads data identified by the given hash from the storage
/// backend. It returns a `Result` which, on success, contains the data as a
/// vector of bytes. On failure, it returns an error.
///
/// - `write_data`: Writes data to the storage backend. The path of the data is
/// chosen based on the provided hash. It returns a `Result` which, on success,
/// contains the `PathBuf` where the data is stored. On failure, it returns an
/// error.
#[async_trait]
pub trait StorageBackend {
    // TODO: Maybe we want to stream it instead? Make this return a reader?
    //
    // Note: this is only r/w'ing a blob (collection of chunks) at a time, so
    // around 8MiB by default ... maybe streaming doesn't make sense here.
    /// Read the data blob identified by the hash from the storage backend.
    async fn read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>>;
    /// Write the data to the storage backend. The path is chosen based on the
    /// hash.
    async fn write_data(
        &self,
        hash: &Hash,
        data: &[u8],
    ) -> Result<PathBuf, Box<dyn std::error::Error>>;
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
        Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Failed to extract file name from path",
        ))
    })?;

    let path_str = file_name.to_str().ok_or_else(|| {
        Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Failed to convert file name to str",
        ))
    })?;

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

    use super::Local;
    use super::StorageBackend;
    use crate::hash::multihash;

    #[tokio::test]
    async fn local_rw_data() {
        let datadir = tempdir().unwrap();
        let storage = Local::new(datadir);

        // Test data
        let data = b"Hello, world!";
        let mh = multihash(data);
        let hash = Hash::from(mh.to_bytes());

        // Write the data
        let pathbuf = storage.write_data(&hash, data).await.unwrap();

        // Read the data back and verify it
        let read_data = storage.read_data(&pathbuf).await.unwrap();
        assert_eq!(data.to_vec(), read_data);
    }
}

// re-export backends
pub use local::Local;
pub use s3::AmazonS3;
