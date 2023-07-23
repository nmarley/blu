// use crate::storage::{Local, StorageBackend};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEFAULT_DATADIR: &str = ".blu/data";

/// Storage backend config for blu. Currently only local filesystem is
/// supported.
#[derive(Debug, PartialEq, Serialize, Deserialize, Eq)]
#[serde(tag = "type")]
pub enum BackendConfig {
    /// Local filesystem
    #[serde(rename = "local")]
    Local(LocalConfig),
    /// Amazon S3
    #[serde(rename = "s3")]
    AmazonS3(S3Config),
}

impl Default for BackendConfig {
    fn default() -> Self {
        BackendConfig::Local(LocalConfig {
            path: PathBuf::from(DEFAULT_DATADIR),
        })
    }
}

/// Configuration for the local filesystem backend.
#[derive(Debug, PartialEq, Serialize, Deserialize, Eq)]
pub struct LocalConfig {
    /// Path to the local filesystem directory where blu will store
    /// encrypted data blobs.
    pub path: PathBuf,
}

/// Configuration for the Amazon S3 backend. Not implemented, currently a
/// placeholder.
// TODO: maybe rename to 'AmazonS3' to avoid confusion for less technical
// users
//
// Note: We have to be careful with storing sensitive data like AWS access keys
// and secret keys in plaintext. It might be better to use AWS's built-in
// mechanisms for managing credentials (like environment variables or IAM
// roles) rather than storing them here in the config file.
#[derive(Debug, PartialEq, Serialize, Deserialize, Eq)]
pub struct S3Config {
    _bucket: String,
    _prefix: Option<String>,
    _access_key: Option<String>,
    _secret_key: Option<String>,
    _region: Option<String>,
}
