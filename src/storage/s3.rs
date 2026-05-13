//! Amazon S3 storage backend implementation.

use aws_sdk_s3::operation::head_object::HeadObjectError;
use aws_sdk_s3::primitives::ByteStream;
use std::path::{Path, PathBuf};

use crate::hash::Hash;

/// Amazon S3 storage backend.
///
/// This backend stores encrypted blob files in an S3 bucket. All I/O
/// is async and driven by the caller's Tokio runtime.
pub struct AmazonS3 {
    bucket: String,
    prefix: PathBuf,
    client: aws_sdk_s3::Client,
}

impl std::fmt::Debug for AmazonS3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AmazonS3")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl AmazonS3 {
    /// Create a new Amazon S3 storage backend with the given bucket name,
    /// optional prefix, and optional region.
    ///
    /// If region is None, it will be determined from the environment
    /// (AWS_REGION, AWS_DEFAULT_REGION) or the AWS config file.
    pub async fn new<P: AsRef<Path>>(
        bucket: &str,
        prefix: Option<P>,
        region: Option<&str>,
    ) -> Self {
        let mut config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest());

        if let Some(r) = region {
            config_loader = config_loader.region(aws_sdk_s3::config::Region::new(r.to_owned()));
        }

        let config = config_loader.load().await;
        let client = aws_sdk_s3::Client::new(&config);

        let prefix = match prefix {
            Some(ref p) => p.as_ref().to_path_buf(),
            None => PathBuf::new(),
        };

        info!("S3 backend: bucket={}, prefix={}", bucket, prefix.display());

        Self {
            bucket: bucket.to_owned(),
            prefix,
            client,
        }
    }

    /// Convert a path to an S3 key string.
    fn path_to_key(&self, path: &Path) -> String {
        self.prefix.join(path).to_string_lossy().to_string()
    }

    /// Read the data blob at the given path from S3.
    pub async fn read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        let object = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let body = object.body.collect().await.map_err(|e| e.to_string())?;
        Ok(body.into_bytes().to_vec())
    }

    /// Write data to a content-addressed path derived from the hash.
    pub async fn write_data(
        &self,
        hash: &Hash,
        data: &[u8],
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = super::path_for(hash)?;
        let key = self.path_to_key(&path);

        info!("S3 write: key={}", key);

        let body = ByteStream::from(data.to_vec());
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        Ok(path)
    }

    /// Check if a blob exists at the given path.
    pub async fn exists(&self, path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        let result = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await;

        match result {
            Ok(_) => Ok(true),
            Err(err) => {
                if let Some(service_err) = err.as_service_error() {
                    if matches!(service_err, HeadObjectError::NotFound(_)) {
                        return Ok(false);
                    }
                }
                Err(Box::new(err) as Box<dyn std::error::Error>)
            }
        }
    }

    /// Delete a blob at the given path.
    pub async fn delete(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Write data to a known path in the backend (not hash-derived).
    pub async fn write_to_path(
        &self,
        path: &Path,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        info!("S3 write_to_path: key={}", key);

        let body = ByteStream::from(data.to_vec());
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Read data from a known path in the backend (not hash-derived).
    pub async fn read_from_path(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        let object = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let body = object.body.collect().await.map_err(|e| e.to_string())?;
        Ok(body.into_bytes().to_vec())
    }
}
