use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::age::BlackBox;
use crate::blob::{BlobIndex, BLOB_INDEX_FILENAME};
use crate::block::{PlainIndex, INDEX_FILENAME};
use crate::error::{BluError, Result as BluResult};
use crate::io::BlackBoxSerializable;
use crate::keys;
use crate::storage::{AmazonS3, Local, StorageBackend};
use crate::tag::{TagIndex, TAG_INDEX_FILENAME};

/// Backend config structures, one for each supported backend.
pub mod backend;

// for now locked to just Age keys, for simplicity
/// KeyType is the type of key used to encrypt/decrypt data. Currently only Age
/// keys are supported.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Hash, Eq, PartialOrd, Ord)]
pub enum KeyType {
    // RSA,
    // DSA,
    // ECDSA,
    // Ed25519,
    /// Age key
    Age,
}

/// KeyID is a unique identifier for a key. It is a combination of the key type
/// and public key, but in reality is just the public key.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Hash, Eq, PartialOrd, Ord)]
pub struct KeyID {
    r#type: KeyType,
    public_key: String, // TODO: Vec<u8> ?
}

/// Encryption configuration for a blu vault.
///
/// The identity (private key) lives at `~/.blu/identity.age` and is
/// resolved at runtime; it is not stored in the vault config.
///
/// Only the PQ recipient is stored. The KEK is wrapped exclusively
/// with the post-quantum hybrid key (ML-KEM-768 + X25519). The
/// X25519 public key is recorded in `~/.blu/identity.toml` (global
/// identity metadata) but is not needed in per-vault config.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Eq, Default)]
pub struct EncryptionConfig {
    /// Post-quantum hybrid recipient (mlkem768x25519).
    /// Format: age1pq...
    pub pq_recipient: String,
}

/// Config is the configuration for blu. It is stored in the .blu directory in
/// the config.(json|toml) file.
#[derive(Debug, PartialEq, Serialize, Deserialize, Eq)]
#[serde(default)]
pub struct Config {
    /// blu version that created this config
    blu_version: String,

    /// Encryption settings (public key, recipients)
    #[serde(default)]
    pub encryption: Option<EncryptionConfig>,

    // base dir (not serialized)
    #[serde(skip)]
    basedir: PathBuf,

    // TODO: multiple backends
    /// Storage backend for encrypted data blobs
    pub backend: backend::BackendConfig,

    // should blu delete Encrypted from filesystem, if the plain version was deleted?
    prune_deleted: bool,
    // should blu delete dangling Encrypted from filesystem?
    prune_dangling: bool,

    plain_index_filename: PathBuf,
    tag_index_filename: PathBuf,
    blob_index_filename: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: backend::BackendConfig::default(),
            blu_version: env!("CARGO_PKG_VERSION").to_string(),
            encryption: None,
            basedir: PathBuf::from("."),
            prune_deleted: false,
            prune_dangling: false,
            plain_index_filename: INDEX_FILENAME.into(),
            tag_index_filename: TAG_INDEX_FILENAME.into(),
            blob_index_filename: BLOB_INDEX_FILENAME.into(),
        }
    }
}

/// Read the vault config from the `.blu/config.toml` in the base directory.
pub fn read_config<P: AsRef<Path>>(base_dir: P) -> Result<Config, Box<dyn std::error::Error>> {
    let config_toml = base_dir.as_ref().join(".blu/config.toml");

    let toml_str = fs::read_to_string(&config_toml)
        .map_err(|_| format!("could not read config at {}", config_toml.display()))?;

    let mut cfg: Config = toml::from_str(&toml_str)?;
    cfg.basedir = base_dir.as_ref().to_path_buf();
    Ok(cfg)
}

/// macro to write load_index, load_tag_index, load_blob_index, etc. ...
macro_rules! load_index {
    // TODO: implement as independent fn in Config, then wrap with impl version pass in path
    ($name: ident, $idx_struct_name:ident, $idx_filename_varname:ident) => {
        /// $name loads the index from the idxdir.
        pub fn $name(&self, bbox: &BlackBox) -> BluResult<$idx_struct_name> {
            let index_path = self.idxdir().join(&self.$idx_filename_varname);
            let index_data: Vec<u8> = fs::read(&index_path).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    BluError::IndexNotFound(index_path.display().to_string())
                }
                _ => BluError::Internal(format!(
                    "failed to read index at {}: {}",
                    index_path.display(),
                    e
                )),
            })?;
            $idx_struct_name::read(&index_data[..], bbox)
                .map_err(|e| BluError::IndexCorrupted(format!("{}: {}", index_path.display(), e)))
        }
    };
}

/// macro to write write_index, write_tag_index, write_blob_index, etc. ...
macro_rules! write_index {
    ($name: ident, $idx_struct_name:ident, $idx_filename_varname:ident) => {
        /// $name writes the index to the idxdir.
        pub fn $name(
            &self,
            idx: &$idx_struct_name,
            bbox: &BlackBox,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let index_path = self.idxdir().join(&self.$idx_filename_varname);
            // encrypt + compress + serialize index to buf
            let mut buf = vec![];
            idx.write(&mut buf, bbox)?;
            // write to file
            std::fs::write(index_path, buf)?;
            Ok(())
        }
    };
}

impl Config {
    /// Returns the .blu dir within the base directory. This holds the config,
    /// and nested indexes and data dirs.
    pub fn bludir(&self) -> PathBuf {
        self.basedir.join(".blu")
    }

    /// Returns the directory used to hold the indexes.
    pub fn idxdir(&self) -> PathBuf {
        self.bludir().join("indexes")
    }

    /// Check if encryption is configured.
    pub fn has_encryption(&self) -> bool {
        self.encryption.is_some()
    }

    /// Load the BlackBox (encryption context) from the identity at the given path.
    ///
    /// If the identity file is passphrase-protected, a passphrase must be provided.
    pub fn load_blackbox(
        &self,
        identity_path: &Path,
        passphrase: Option<&str>,
    ) -> BluResult<BlackBox> {
        let identity = keys::load_identity(identity_path, passphrase)?;
        Ok(keys::blackbox_from_identity(identity))
    }

    /// Set the encryption configuration.
    pub fn set_encryption(&mut self, encryption: EncryptionConfig) {
        self.encryption = Some(encryption);
    }

    /// Get the base directory for the vault.
    pub fn basedir(&self) -> &Path {
        &self.basedir
    }

    load_index!(load_blob_index, BlobIndex, blob_index_filename);
    load_index!(load_tag_index, TagIndex, tag_index_filename);
    load_index!(load_plain_index, PlainIndex, plain_index_filename);

    write_index!(write_blob_index, BlobIndex, blob_index_filename);
    write_index!(write_tag_index, TagIndex, tag_index_filename);
    write_index!(write_plain_index, PlainIndex, plain_index_filename);

    /// Initializes the storage backend based on `backend` field in config.
    pub fn init_storage_backend(
        &self,
    ) -> Result<Box<dyn StorageBackend>, Box<dyn std::error::Error>> {
        match self.backend {
            backend::BackendConfig::Local(ref local_backend) => {
                Ok(Box::new(Local::new(&local_backend.path)))
            }
            backend::BackendConfig::AmazonS3(ref s3_backend) => Ok(Box::new(AmazonS3::new(
                &s3_backend.bucket,
                s3_backend.prefix.as_deref(),
                s3_backend.region.as_deref(),
            ))),
            #[allow(unreachable_patterns)]
            _ => Err("Unsupported backend".into()),
        }
    }

    /// Remote path for the plain index file in the backend.
    fn remote_plain_index_path(&self) -> PathBuf {
        PathBuf::from("indexes").join(&self.plain_index_filename)
    }

    /// Remote path for the blob index file in the backend.
    fn remote_blob_index_path(&self) -> PathBuf {
        PathBuf::from("indexes").join(&self.blob_index_filename)
    }

    /// Remote path for the tag index file in the backend.
    fn remote_tag_index_path(&self) -> PathBuf {
        PathBuf::from("indexes").join(&self.tag_index_filename)
    }

    /// Push local indexes to the remote backend.
    ///
    /// This uploads the encrypted index files to the backend, making them
    /// accessible from other machines with the same key.
    pub fn push_indexes(
        &self,
        backend: &dyn StorageBackend,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Read local index files
        let plain_index_path = self.idxdir().join(&self.plain_index_filename);
        let blob_index_path = self.idxdir().join(&self.blob_index_filename);

        // Upload plain index
        if plain_index_path.exists() {
            let data = fs::read(&plain_index_path)?;
            let hash = crate::hash::Hash::from(crate::hash::multihash(&data).to_bytes());
            let remote_path = self.remote_plain_index_path();
            info!("Pushing plain index to {:?}", remote_path);
            backend.write_data(&hash, &data)?;
            // Also write to the known index path (not hash-based)
            self.write_index_to_backend(backend, &data, &remote_path)?;
        }

        // Upload blob index
        if blob_index_path.exists() {
            let data = fs::read(&blob_index_path)?;
            let remote_path = self.remote_blob_index_path();
            info!("Pushing blob index to {:?}", remote_path);
            self.write_index_to_backend(backend, &data, &remote_path)?;
        }

        // Upload tag index if it exists
        let tag_index_path = self.idxdir().join(&self.tag_index_filename);
        if tag_index_path.exists() {
            let data = fs::read(&tag_index_path)?;
            let remote_path = self.remote_tag_index_path();
            info!("Pushing tag index to {:?}", remote_path);
            self.write_index_to_backend(backend, &data, &remote_path)?;
        }

        Ok(())
    }

    /// Helper to write index data to a specific path in the backend.
    fn write_index_to_backend(
        &self,
        backend: &dyn StorageBackend,
        data: &[u8],
        path: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // For index files, we write directly to a known path, not hash-based
        // We need to use the hash to satisfy the trait, but we'll use the path
        let hash = crate::hash::Hash::from(crate::hash::multihash(data).to_bytes());

        // Write to the backend - for local backend this works fine
        // For S3, we need a different approach since write_data uses hash-based paths
        match &self.backend {
            backend::BackendConfig::Local(ref local_backend) => {
                let full_path = local_backend.path.join(path);
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(full_path, data)?;
            }
            backend::BackendConfig::AmazonS3(_) => {
                // For S3, we write to indexes/ prefix
                // The S3 backend's write_data uses hash-based paths, so we need
                // to write index files specially. For now, we'll write them
                // to a hash-based path and also maintain a manifest.
                // TODO: Add a write_to_path method to StorageBackend trait
                backend.write_data(&hash, data)?;
            }
            #[allow(unreachable_patterns)]
            _ => return Err("Unsupported backend".into()),
        }
        Ok(())
    }

    /// Pull indexes from the remote backend.
    ///
    /// This downloads the encrypted index files from the backend,
    /// overwriting local indexes.
    pub fn pull_indexes(
        &self,
        backend: &dyn StorageBackend,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // For local backend, read from the known paths
        // For S3, we need to know where the indexes are stored
        match &self.backend {
            backend::BackendConfig::Local(ref local_backend) => {
                // Plain index
                let remote_plain = local_backend.path.join(self.remote_plain_index_path());
                if remote_plain.exists() {
                    let data = fs::read(&remote_plain)?;
                    let local_path = self.idxdir().join(&self.plain_index_filename);
                    fs::write(local_path, data)?;
                    info!("Pulled plain index");
                }

                // Blob index
                let remote_blob = local_backend.path.join(self.remote_blob_index_path());
                if remote_blob.exists() {
                    let data = fs::read(&remote_blob)?;
                    let local_path = self.idxdir().join(&self.blob_index_filename);
                    fs::write(local_path, data)?;
                    info!("Pulled blob index");
                }

                // Tag index
                let remote_tag = local_backend.path.join(self.remote_tag_index_path());
                if remote_tag.exists() {
                    let data = fs::read(&remote_tag)?;
                    let local_path = self.idxdir().join(&self.tag_index_filename);
                    fs::write(local_path, data)?;
                    info!("Pulled tag index");
                }
            }
            backend::BackendConfig::AmazonS3(_) => {
                // For S3, read from the indexes/ prefix
                let remote_plain = self.remote_plain_index_path();
                if backend.exists(&remote_plain)? {
                    let data = backend.read_data(&remote_plain)?;
                    let local_path = self.idxdir().join(&self.plain_index_filename);
                    fs::write(local_path, data)?;
                    info!("Pulled plain index from S3");
                }

                let remote_blob = self.remote_blob_index_path();
                if backend.exists(&remote_blob)? {
                    let data = backend.read_data(&remote_blob)?;
                    let local_path = self.idxdir().join(&self.blob_index_filename);
                    fs::write(local_path, data)?;
                    info!("Pulled blob index from S3");
                }

                let remote_tag = self.remote_tag_index_path();
                if backend.exists(&remote_tag)? {
                    let data = backend.read_data(&remote_tag)?;
                    let local_path = self.idxdir().join(&self.tag_index_filename);
                    fs::write(local_path, data)?;
                    info!("Pulled tag index from S3");
                }
            }
            #[allow(unreachable_patterns)]
            _ => return Err("Unsupported backend".into()),
        }

        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test {
    use super::Config;

    #[test]
    fn read_config_missing_dir_errors() {
        assert!(super::read_config("test/old/t0/").is_err());
    }

    #[test]
    fn read_config_json_only_errors() {
        // Legacy JSON configs are no longer supported
        assert!(super::read_config("test/old/t1/").is_err());
    }

    #[test]
    fn config_toml_round_trip() {
        let mut cfg = Config::default();
        cfg.set_encryption(super::EncryptionConfig {
            pq_recipient: "age1pqtest".to_string(),
        });

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        let enc = parsed.encryption.unwrap();
        assert_eq!(enc.pq_recipient, "age1pqtest");
    }
}
