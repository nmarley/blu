use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::blob::{BlobIndex, BLOB_INDEX_FILENAME};
use crate::block::{PlainIndex, INDEX_FILENAME};
use crate::dek_provider::DekProvider;
use crate::error::{BluError, Result as BluResult};
use crate::index_merge::{merge_blob_index, merge_plain_index, merge_tag_index, PathConflict};
use crate::io::EncryptedSerializable;
use crate::storage::{AmazonS3, BackendKind, Local};
use crate::tag::{TagIndex, TAG_INDEX_FILENAME};

/// Backend config structures, one for each supported backend.
pub mod backend;

/// Summary of a merge of remote indexes into the local vault.
#[derive(Debug, Clone, Default)]
pub struct IndexMergeSummary {
    /// Path conflicts detected while merging plain indexes.
    pub conflicts: Vec<PathConflict>,
    /// True when remote indexes existed and were merged.
    pub merged: bool,
}

/// Encryption configuration for a blu vault.
///
/// The identity (private key) lives at
/// `$XDG_DATA_HOME/blu/identity.age` and is resolved at runtime; it is
/// not stored in the vault config.
///
/// Only the PQ recipient is stored. The KEK is wrapped exclusively
/// with the post-quantum hybrid key (ML-KEM-768 + X25519). The
/// PQ public key is recorded in `$XDG_DATA_HOME/blu/identity.toml`
/// (global identity metadata) and copied into each vault config.
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

    /// Named storage backends.
    #[serde(default = "backend::default_backends")]
    pub backends: HashMap<String, backend::BackendConfig>,

    /// Name of the default backend for reads and writes.
    #[serde(default = "backend::default_backend_name")]
    pub default_backend: String,

    /// Legacy singular backend (deprecated).
    /// Present only when deserializing old-format configs that use
    /// `[backend]` instead of `[backends.<name>]`.
    #[serde(default, skip_serializing)]
    backend: Option<backend::BackendConfig>,

    plain_index_filename: PathBuf,
    tag_index_filename: PathBuf,
    blob_index_filename: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backends: backend::default_backends(),
            default_backend: backend::default_backend_name(),
            backend: None,
            blu_version: env!("CARGO_PKG_VERSION").to_string(),
            encryption: None,
            basedir: PathBuf::from("."),
            plain_index_filename: INDEX_FILENAME.into(),
            tag_index_filename: TAG_INDEX_FILENAME.into(),
            blob_index_filename: BLOB_INDEX_FILENAME.into(),
        }
    }
}

/// Read the vault config from the `.blu/config.toml` in the base directory.
pub fn read_config<P: AsRef<Path>>(base_dir: P) -> Result<Config, BluError> {
    let base_dir = base_dir.as_ref();
    let config_toml = base_dir.join(".blu/config.toml");

    let toml_str = fs::read_to_string(&config_toml).map_err(|_| {
        BluError::InvalidConfig(format!(
            "could not read config at {}",
            config_toml.display()
        ))
    })?;

    let mut cfg: Config = toml::from_str(&toml_str)?;
    cfg.basedir = base_dir.canonicalize().map_err(|e| {
        BluError::InvalidConfig(format!(
            "could not resolve blu repository at {}: {}",
            base_dir.display(),
            e
        ))
    })?;
    cfg.resolve_backends()?;
    Ok(cfg)
}

/// Macro to generate load methods for each index type.
macro_rules! load_index {
    ($name: ident, $idx_struct_name:ident, $idx_filename_varname:ident) => {
        /// Load the index from the idxdir.
        pub fn $name(&self, keys: &DekProvider) -> BluResult<$idx_struct_name> {
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
            $idx_struct_name::read(&index_data[..], keys).map_err(|e| BluError::IndexLoadFailed {
                path: index_path.clone(),
                reason: e.to_string(),
            })
        }
    };
}

/// Macro to generate graceful index-load methods that return a default
/// value when the index is missing or has an incompatible format.
///
/// This is the right choice for supplementary indexes (blob, tag) and
/// for commands that can rebuild the index from scratch (sync, add).
macro_rules! load_index_or_default {
    ($name: ident, $idx_struct_name:ident, $idx_filename_varname:ident, $default_expr:expr) => {
        /// Load the index, returning a default value if the file is
        /// missing or cannot be deserialized (e.g. format migration).
        pub fn $name(&self, keys: &DekProvider) -> $idx_struct_name {
            let index_path = self.idxdir().join(&self.$idx_filename_varname);
            let index_data = match fs::read(&index_path) {
                Ok(data) => data,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return $default_expr;
                }
                Err(e) => {
                    log::warn!(
                        "Cannot read index at {}: {}, using empty default",
                        index_path.display(),
                        e
                    );
                    return $default_expr;
                }
            };
            match $idx_struct_name::read(&index_data[..], keys) {
                Ok(idx) => idx,
                Err(e) => {
                    log::warn!(
                        "Index at {} unreadable ({}), using empty default",
                        index_path.display(),
                        e
                    );
                    $default_expr
                }
            }
        }
    };
}

/// Macro to generate write methods for each index type.
macro_rules! write_index {
    ($name: ident, $idx_struct_name:ident, $idx_filename_varname:ident) => {
        /// Write the index to the idxdir.
        pub fn $name(&self, idx: &$idx_struct_name, keys: &DekProvider) -> Result<(), BluError> {
            let index_path = self.idxdir().join(&self.$idx_filename_varname);
            // encrypt + compress + serialize index to buf
            let mut buf = vec![];
            idx.write(&mut buf, keys)?;
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

    /// Set the encryption configuration.
    pub fn set_encryption(&mut self, encryption: EncryptionConfig) {
        self.encryption = Some(encryption);
    }

    /// Get the base directory for the vault.
    pub fn basedir(&self) -> &Path {
        &self.basedir
    }

    /// Set the base directory for the vault. Normally set by
    /// [`read_config`] during vault discovery; exposed for tests
    /// and tooling that construct a `Config` without a TOML file.
    pub fn set_basedir(&mut self, basedir: PathBuf) {
        self.basedir = basedir;
    }

    /// Write the config back to `.blu/config.toml`.
    pub fn save(&self) -> Result<(), BluError> {
        let config_path = self.bludir().join("config.toml");
        let toml_str = toml::to_string_pretty(self)?;
        fs::write(config_path, toml_str)?;
        Ok(())
    }

    /// Promote a legacy `[backend]` section into the named backends
    /// map. Called from `read_config` after deserialization.
    ///
    /// If the legacy `backend` field is present and `backends` is at
    /// its default (single "default" local entry), the legacy value
    /// replaces it. A deprecation notice is emitted to stderr.
    ///
    /// Returns an error if `default_backend` names a key that does
    /// not exist in `backends`.
    fn resolve_backends(&mut self) -> Result<(), BluError> {
        if let Some(legacy) = self.backend.take() {
            // Old-format config: promote into the named map.
            self.backends.clear();
            self.backends
                .insert(backend::LEGACY_BACKEND_NAME.to_string(), legacy);
            self.default_backend = backend::LEGACY_BACKEND_NAME.to_string();
            eprintln!(
                "warning: deprecated config format: `[backend]` should be \
                 migrated to `[backends.default]`"
            );
        }

        if self.backends.is_empty() {
            return Err(BluError::InvalidConfig("no backends configured".into()));
        }

        if !self.backends.contains_key(&self.default_backend) {
            return Err(BluError::InvalidConfig(format!(
                "default_backend \"{}\" not found in [backends]",
                self.default_backend
            )));
        }

        Ok(())
    }

    load_index!(load_blob_index, BlobIndex, blob_index_filename);
    load_index!(load_tag_index, TagIndex, tag_index_filename);
    load_index!(load_plain_index, PlainIndex, plain_index_filename);

    load_index_or_default!(
        load_blob_index_or_default,
        BlobIndex,
        blob_index_filename,
        BlobIndex::default()
    );
    load_index_or_default!(
        load_tag_index_or_default,
        TagIndex,
        tag_index_filename,
        TagIndex::default()
    );
    load_index_or_default!(
        load_plain_index_or_default,
        PlainIndex,
        plain_index_filename,
        PlainIndex::new_empty()
    );

    write_index!(write_blob_index, BlobIndex, blob_index_filename);
    write_index!(write_tag_index, TagIndex, tag_index_filename);
    write_index!(write_plain_index, PlainIndex, plain_index_filename);

    /// Construct a [`BackendKind`] from a [`BackendConfig`](backend::BackendConfig).
    async fn build_backend(cfg: &backend::BackendConfig) -> Result<BackendKind, BluError> {
        match cfg {
            backend::BackendConfig::Local(ref local_backend) => {
                Ok(BackendKind::Local(Local::new(&local_backend.path)))
            }
            backend::BackendConfig::AmazonS3(ref s3_backend) => Ok(BackendKind::AmazonS3(
                AmazonS3::new(
                    &s3_backend.bucket,
                    s3_backend.prefix.as_deref(),
                    s3_backend.region.as_deref(),
                )
                .await,
            )),
        }
    }

    /// Initializes the default storage backend.
    pub async fn init_storage_backend(&self) -> Result<BackendKind, BluError> {
        self.init_named_backend(&self.default_backend).await
    }

    /// Initializes a storage backend by name.
    pub async fn init_named_backend(&self, name: &str) -> Result<BackendKind, BluError> {
        let cfg = self.backends.get(name).ok_or_else(|| {
            BluError::InvalidConfig(format!("backend \"{}\" not found in config", name))
        })?;
        Self::build_backend(cfg).await
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

    /// Push local indexes and KEK store to the remote backend.
    ///
    /// Uploads encrypted index files and the UK-wrapped KEK store so
    /// another machine with the same identity can open the vault from
    /// the backend alone.
    pub async fn push_indexes(&self, backend: &BackendKind) -> Result<(), BluError> {
        // Read local index data (synchronous fs reads are fast)
        let plain = self.read_local_index(&self.plain_index_filename);
        let blob = self.read_local_index(&self.blob_index_filename);
        let tag = self.read_local_index(&self.tag_index_filename);

        // Upload all indexes concurrently
        let (r_plain, r_blob, r_tag) = tokio::join!(
            self.push_one_file(
                backend,
                plain,
                self.remote_plain_index_path(),
                "plain index"
            ),
            self.push_one_file(backend, blob, self.remote_blob_index_path(), "blob index"),
            self.push_one_file(backend, tag, self.remote_tag_index_path(), "tag index"),
        );
        r_plain?;
        r_blob?;
        r_tag?;

        self.push_kek_store(backend).await?;

        Ok(())
    }

    /// Read a local index file, returning None if it does not exist.
    fn read_local_index(&self, filename: &Path) -> Option<Vec<u8>> {
        let path = self.idxdir().join(filename);
        if path.exists() {
            fs::read(&path).ok()
        } else {
            None
        }
    }

    /// Push a single file to the backend (no-op if data is None).
    async fn push_one_file(
        &self,
        backend: &BackendKind,
        data: Option<Vec<u8>>,
        remote_path: PathBuf,
        label: &str,
    ) -> Result<(), BluError> {
        if let Some(data) = data {
            info!("Pushing {} to {:?}", label, remote_path);
            backend.write_to_path(&remote_path, &data).await?;
        }
        Ok(())
    }

    /// Push the local KEK store (`keys/kek.toml` + wrapped KEKs).
    ///
    /// No-op when no local KEK store exists. Objects are already
    /// age-wrapped to authorized UKs; they are uploaded as opaque
    /// ciphertext.
    async fn push_kek_store(&self, backend: &BackendKind) -> Result<(), BluError> {
        let store = crate::keys::kek::KekStore::new(&self.bludir());
        if !store.exists() {
            return Ok(());
        }

        let metadata = store.load_metadata()?;
        let meta_local = self.bludir().join("keys/kek.toml");
        let meta_bytes = fs::read(&meta_local)?;
        self.push_one_file(
            backend,
            Some(meta_bytes),
            PathBuf::from("keys/kek.toml"),
            "kek metadata",
        )
        .await?;

        for version in &metadata.versions {
            let local = self
                .bludir()
                .join(format!("keys/kek_v{}/wrapped.age", version.version));
            let remote = PathBuf::from(format!("keys/kek_v{}/wrapped.age", version.version));
            if local.exists() {
                let data = fs::read(&local)?;
                self.push_one_file(
                    backend,
                    Some(data),
                    remote,
                    &format!("kek v{}", version.version),
                )
                .await?;
            }
        }

        Ok(())
    }

    /// Pull indexes and KEK store from the remote backend.
    ///
    /// Downloads encrypted index files and the UK-wrapped KEK store,
    /// overwriting local copies when present remotely.
    pub async fn pull_indexes(&self, backend: &BackendKind) -> Result<(), BluError> {
        let (r_plain, r_blob, r_tag) = tokio::join!(
            self.pull_one_file(
                backend,
                self.remote_plain_index_path(),
                self.idxdir().join(&self.plain_index_filename),
            ),
            self.pull_one_file(
                backend,
                self.remote_blob_index_path(),
                self.idxdir().join(&self.blob_index_filename),
            ),
            self.pull_one_file(
                backend,
                self.remote_tag_index_path(),
                self.idxdir().join(&self.tag_index_filename),
            ),
        );
        r_plain?;
        r_blob?;
        r_tag?;

        self.pull_kek_store(backend).await?;

        Ok(())
    }

    /// Fetch remote indexes (if any), union-merge into local, rewrite local.
    ///
    /// No-op when the remote has no plain or blob index. Used before push
    /// so concurrent multi-device adds are preserved, and by pull (merge
    /// mode) so local-only entries are not discarded.
    pub async fn merge_remote_indexes(
        &self,
        backend: &BackendKind,
        keys: &DekProvider,
    ) -> Result<IndexMergeSummary, BluError> {
        let remote_plain_path = self.remote_plain_index_path();
        let remote_blob_path = self.remote_blob_index_path();
        let remote_tag_path = self.remote_tag_index_path();

        let has_remote = backend.exists(&remote_plain_path).await?
            || backend.exists(&remote_blob_path).await?
            || backend.exists(&remote_tag_path).await?;
        if !has_remote {
            return Ok(IndexMergeSummary::default());
        }

        let local_plain = self.load_plain_index_or_default(keys);
        let local_blob = self.load_blob_index_or_default(keys);
        let local_tag = self.load_tag_index_or_default(keys);

        let remote_plain =
            Self::read_remote_index::<PlainIndex>(backend, &remote_plain_path, keys).await?;
        let remote_blob =
            Self::read_remote_index::<BlobIndex>(backend, &remote_blob_path, keys).await?;
        let remote_tag =
            Self::read_remote_index::<TagIndex>(backend, &remote_tag_path, keys).await?;

        let mut conflicts = Vec::new();

        let plain = match remote_plain {
            Some(remote) => {
                let merged = merge_plain_index(&local_plain, &remote)?;
                conflicts = merged.conflicts;
                merged.index
            }
            None => local_plain,
        };
        let blob = match remote_blob {
            Some(remote) => merge_blob_index(&local_blob, &remote),
            None => local_blob,
        };
        let tag = match remote_tag {
            Some(remote) => merge_tag_index(&local_tag, &remote),
            None => local_tag,
        };

        self.write_plain_index(&plain, keys)?;
        self.write_blob_index(&blob, keys)?;
        self.write_tag_index(&tag, keys)?;

        Ok(IndexMergeSummary {
            conflicts,
            merged: true,
        })
    }

    /// Pull indexes by union-merging remote into local (not overwrite).
    ///
    /// Also refreshes the KEK store from the backend.
    pub async fn pull_indexes_merged(
        &self,
        backend: &BackendKind,
        keys: &DekProvider,
    ) -> Result<IndexMergeSummary, BluError> {
        let summary = self.merge_remote_indexes(backend, keys).await?;
        self.pull_kek_store(backend).await?;
        Ok(summary)
    }

    async fn read_remote_index<T: EncryptedSerializable>(
        backend: &BackendKind,
        remote_path: &Path,
        keys: &DekProvider,
    ) -> Result<Option<T>, BluError> {
        if !backend.exists(remote_path).await? {
            return Ok(None);
        }
        let data = backend.read_from_path(remote_path).await?;
        let idx = T::read(&data[..], keys).map_err(|e| BluError::IndexLoadFailed {
            path: remote_path.to_path_buf(),
            reason: e.to_string(),
        })?;
        Ok(Some(idx))
    }

    /// Pull a single file from the backend if it exists remotely.
    async fn pull_one_file(
        &self,
        backend: &BackendKind,
        remote_path: PathBuf,
        local_path: PathBuf,
    ) -> Result<(), BluError> {
        if backend.exists(&remote_path).await? {
            let data = backend.read_from_path(&remote_path).await?;
            if let Some(parent) = local_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&local_path, data)?;
            info!("Pulled {:?}", remote_path);
        }
        Ok(())
    }

    /// Pull the KEK store from the backend.
    ///
    /// Fetches `keys/kek.toml` first (manifest of versions), then each
    /// `keys/kek_vN/wrapped.age`. No-op when remote metadata is absent
    /// (older backends that never published keys).
    async fn pull_kek_store(&self, backend: &BackendKind) -> Result<(), BluError> {
        let remote_meta = PathBuf::from("keys/kek.toml");
        if !backend.exists(&remote_meta).await? {
            return Ok(());
        }

        let meta_bytes = backend.read_from_path(&remote_meta).await?;
        let metadata: crate::keys::kek::KekMetadata = toml::from_str(
            std::str::from_utf8(&meta_bytes)
                .map_err(|e| BluError::InvalidConfig(format!("remote kek.toml: {}", e)))?,
        )
        .map_err(|e| BluError::InvalidConfig(format!("remote kek.toml: {}", e)))?;

        let local_meta = self.bludir().join("keys/kek.toml");
        if let Some(parent) = local_meta.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&local_meta, &meta_bytes)?;
        info!("Pulled {:?}", remote_meta);

        for version in &metadata.versions {
            let remote = PathBuf::from(format!("keys/kek_v{}/wrapped.age", version.version));
            let local = self
                .bludir()
                .join(format!("keys/kek_v{}/wrapped.age", version.version));
            self.pull_one_file(backend, remote, local).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test {
    use super::backend::BackendConfig;
    use super::Config;
    use std::fs;

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

    #[test]
    fn read_config_uses_canonical_basedir() {
        let current_dir = std::env::current_dir().unwrap();
        let tmp = tempfile::tempdir_in(&current_dir).unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".blu")).unwrap();

        let mut cfg = Config::default();
        cfg.set_encryption(super::EncryptionConfig {
            pq_recipient: "age1pqtest".to_string(),
        });
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        fs::write(repo.join(".blu/config.toml"), toml_str).unwrap();

        let relative_repo = repo.strip_prefix(&current_dir).unwrap();
        let loaded = super::read_config(relative_repo).unwrap();

        assert_eq!(loaded.bludir(), repo.canonicalize().unwrap().join(".blu"));
    }

    #[test]
    fn legacy_backend_promoted_to_named_map() {
        let toml_str = r#"
            blu_version = "0.5.0"
            [backend]
            type = "local"
            path = ".blu/data"
        "#;
        let mut cfg: Config = toml::from_str(toml_str).unwrap();
        cfg.resolve_backends().unwrap();

        assert_eq!(cfg.default_backend, "default");
        assert_eq!(cfg.backends.len(), 1);
        assert!(cfg.backends.contains_key("default"));
        assert!(cfg.backend.is_none());
    }

    #[test]
    fn new_format_with_two_backends_parses() {
        let toml_str = r#"
            blu_version = "0.5.0"
            default_backend = "s3-prod"

            [backends.local]
            type = "local"
            path = ".blu/data"

            [backends.s3-prod]
            type = "s3"
            bucket = "my-bucket"
            region = "us-east-1"
        "#;
        let mut cfg: Config = toml::from_str(toml_str).unwrap();
        cfg.resolve_backends().unwrap();

        assert_eq!(cfg.default_backend, "s3-prod");
        assert_eq!(cfg.backends.len(), 2);
        assert!(cfg.backends.contains_key("local"));
        assert!(cfg.backends.contains_key("s3-prod"));

        match &cfg.backends["s3-prod"] {
            BackendConfig::AmazonS3(s3) => {
                assert_eq!(s3.bucket, "my-bucket");
                assert_eq!(s3.region.as_deref(), Some("us-east-1"));
            }
            _ => panic!("expected S3 backend config"),
        }
    }

    #[test]
    fn missing_default_backend_is_error() {
        let toml_str = r#"
            blu_version = "0.5.0"
            default_backend = "nonexistent"

            [backends.local]
            type = "local"
            path = ".blu/data"
        "#;
        let mut cfg: Config = toml::from_str(toml_str).unwrap();
        let err = cfg.resolve_backends().unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "expected 'not found' error, got: {}",
            err
        );
    }

    #[test]
    fn default_config_serializes_new_format() {
        let cfg = Config::default();
        let toml_str = toml::to_string_pretty(&cfg).unwrap();

        // New format uses [backends.default], not [backend]
        assert!(
            toml_str.contains("[backends.default]"),
            "expected [backends.default] in serialized config, got:\n{}",
            toml_str
        );
        assert!(
            !toml_str.contains("\n[backend]\n"),
            "legacy [backend] should not appear in serialized config"
        );
    }

    #[test]
    fn new_format_round_trips() {
        let cfg = Config::default();
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let mut parsed: Config = toml::from_str(&toml_str).unwrap();
        parsed.resolve_backends().unwrap();

        assert_eq!(parsed.default_backend, "default");
        assert_eq!(parsed.backends.len(), 1);
        assert!(parsed.backends.contains_key("default"));
    }

    #[tokio::test]
    async fn init_named_backend_unknown_name_errors() {
        let cfg = Config::default();
        let result = cfg.init_named_backend("bogus").await;
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(
            err.to_string().contains("not found"),
            "expected 'not found' error, got: {}",
            err
        );
    }

    /// Build a Config with a local backend at `datadir` and basedir `vault`.
    fn local_backend_config(datadir: &std::path::Path, vault: &std::path::Path) -> Config {
        let toml_str = format!(
            r#"
            blu_version = "0.7.0"
            default_backend = "local"

            [backends.local]
            type = "local"
            path = "{}"
            "#,
            datadir.display()
        );
        let mut cfg: Config = toml::from_str(&toml_str).unwrap();
        cfg.set_basedir(vault.to_path_buf());
        cfg
    }

    #[tokio::test]
    async fn push_indexes_uploads_kek_store() {
        use crate::keys::kek::KekStore;
        use crate::keys::mnemonic;

        let tmp = tempfile::tempdir().unwrap();
        let datadir = tmp.path().join("data");
        let vault = tmp.path().join("vault");
        fs::create_dir_all(vault.join(".blu")).unwrap();

        let m = mnemonic::parse_mnemonic(mnemonic::TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");
        let pq_recipient = mnemonic::derive_pq_recipient(&seed).unwrap();
        let user = pq_recipient.to_string();
        let recipients: Vec<&dyn age::Recipient> = vec![&pq_recipient as &dyn age::Recipient];

        let store = KekStore::new(&vault.join(".blu"));
        store.init_with(&recipients, &[user]).unwrap();

        let cfg = local_backend_config(&datadir, &vault);
        let idxdir = cfg.idxdir();
        fs::create_dir_all(&idxdir).unwrap();
        fs::write(idxdir.join("index.dat"), b"plain").unwrap();

        cfg.push_indexes(&cfg.init_storage_backend().await.unwrap())
            .await
            .unwrap();

        assert_eq!(
            fs::read(datadir.join("indexes/index.dat")).unwrap(),
            b"plain"
        );
        assert!(datadir.join("keys/kek.toml").exists());
        assert!(datadir.join("keys/kek_v0/wrapped.age").exists());

        let local_meta = fs::read(vault.join(".blu/keys/kek.toml")).unwrap();
        let remote_meta = fs::read(datadir.join("keys/kek.toml")).unwrap();
        assert_eq!(local_meta, remote_meta);

        let local_wrapped = fs::read(vault.join(".blu/keys/kek_v0/wrapped.age")).unwrap();
        let remote_wrapped = fs::read(datadir.join("keys/kek_v0/wrapped.age")).unwrap();
        assert_eq!(local_wrapped, remote_wrapped);
    }

    #[tokio::test]
    async fn pull_indexes_downloads_kek_store() {
        use crate::keys::kek::KekStore;
        use crate::keys::mnemonic;

        let tmp = tempfile::tempdir().unwrap();
        let datadir = tmp.path().join("data");
        let source_vault = tmp.path().join("source");
        let dest_vault = tmp.path().join("dest");
        fs::create_dir_all(source_vault.join(".blu")).unwrap();
        fs::create_dir_all(dest_vault.join(".blu")).unwrap();

        let m = mnemonic::parse_mnemonic(mnemonic::TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");
        let pq_recipient = mnemonic::derive_pq_recipient(&seed).unwrap();
        let user = pq_recipient.to_string();
        let recipients: Vec<&dyn age::Recipient> = vec![&pq_recipient as &dyn age::Recipient];

        let store = KekStore::new(&source_vault.join(".blu"));
        store.init_with(&recipients, &[user]).unwrap();

        let source_cfg = local_backend_config(&datadir, &source_vault);
        let idxdir = source_cfg.idxdir();
        fs::create_dir_all(&idxdir).unwrap();
        fs::write(idxdir.join("index.dat"), b"plain-index").unwrap();
        fs::write(idxdir.join("blob_index.dat"), b"blob-index").unwrap();

        let backend = source_cfg.init_storage_backend().await.unwrap();
        source_cfg.push_indexes(&backend).await.unwrap();

        let dest_cfg = local_backend_config(&datadir, &dest_vault);
        dest_cfg.pull_indexes(&backend).await.unwrap();

        assert_eq!(
            fs::read(dest_vault.join(".blu/indexes/index.dat")).unwrap(),
            b"plain-index"
        );
        assert_eq!(
            fs::read(dest_vault.join(".blu/indexes/blob_index.dat")).unwrap(),
            b"blob-index"
        );

        let source_meta = fs::read(source_vault.join(".blu/keys/kek.toml")).unwrap();
        let dest_meta = fs::read(dest_vault.join(".blu/keys/kek.toml")).unwrap();
        assert_eq!(source_meta, dest_meta);

        let source_wrapped = fs::read(source_vault.join(".blu/keys/kek_v0/wrapped.age")).unwrap();
        let dest_wrapped = fs::read(dest_vault.join(".blu/keys/kek_v0/wrapped.age")).unwrap();
        assert_eq!(source_wrapped, dest_wrapped);

        // Destination KEK store must be unwrapable with the same identity.
        let pq_identity = mnemonic::derive_pq_identity(&seed).unwrap();
        let dest_store = KekStore::new(&dest_vault.join(".blu"));
        let (kek, version) = dest_store
            .unwrap_current_kek_with(&[&pq_identity as &dyn age::Identity])
            .unwrap();
        assert_eq!(version, 0);
        assert_eq!(kek.as_bytes().len(), 32);
    }

    #[tokio::test]
    async fn pull_indexes_without_remote_kek_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let datadir = tmp.path().join("data");
        let vault = tmp.path().join("vault");
        fs::create_dir_all(vault.join(".blu")).unwrap();

        // Seed backend with indexes only (legacy shape, no keys/).
        fs::create_dir_all(datadir.join("indexes")).unwrap();
        fs::write(datadir.join("indexes/index.dat"), b"legacy").unwrap();

        let cfg = local_backend_config(&datadir, &vault);
        let backend = cfg.init_storage_backend().await.unwrap();
        cfg.pull_indexes(&backend).await.unwrap();

        assert_eq!(
            fs::read(vault.join(".blu/indexes/index.dat")).unwrap(),
            b"legacy"
        );
        assert!(!vault.join(".blu/keys/kek.toml").exists());
    }
}
