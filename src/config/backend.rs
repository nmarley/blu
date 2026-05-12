// use crate::storage::{Local, Backend};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEFAULT_DATADIR: &str = ".blu/data";

/// Storage backend config for blu.
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

/// Configuration for the Amazon S3 backend.
///
/// Note: AWS credentials are loaded from the environment (AWS_ACCESS_KEY_ID,
/// AWS_SECRET_ACCESS_KEY) or from IAM roles. Do not store credentials in the
/// config file.
#[derive(Debug, PartialEq, Serialize, Deserialize, Eq, Clone)]
pub struct S3Config {
    /// The S3 bucket to store the data
    pub bucket: String,
    /// An optional prefix for the S3 object key (e.g., "backups/photos")
    pub prefix: Option<String>,
    /// AWS region (e.g., "us-east-1"). If not specified, uses AWS_REGION
    /// environment variable or default region from AWS config.
    pub region: Option<String>,
}
