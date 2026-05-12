//! Amazon S3 storage backend implementation.

use aws_sdk_s3::operation::head_object::HeadObjectError;
use aws_sdk_s3::primitives::ByteStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::runtime::Runtime;

use crate::hash::Hash;

use super::Backend;

/// Amazon S3 storage backend.
///
/// This backend stores encrypted blob files in an S3 bucket. The runtime is
/// created once and shared across all operations for efficiency.
pub struct AmazonS3 {
    bucket: String,
    prefix: PathBuf,
    client: aws_sdk_s3::Client,
    runtime: Arc<Runtime>,
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
    pub fn new<P: AsRef<Path>>(bucket: &str, prefix: Option<P>, region: Option<&str>) -> Self {
        let runtime = Arc::new(Runtime::new().expect("failed to create tokio runtime"));

        let client = runtime.block_on(async {
            let mut config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest());

            // Set region if provided
            if let Some(r) = region {
                config_loader = config_loader.region(aws_sdk_s3::config::Region::new(r.to_owned()));
            }

            let config = config_loader.load().await;
            aws_sdk_s3::Client::new(&config)
        });

        let prefix = match prefix {
            Some(ref p) => p.as_ref().to_path_buf(),
            None => PathBuf::new(),
        };

        info!("S3 backend: bucket={}, prefix={}", bucket, prefix.display());

        Self {
            bucket: bucket.to_owned(),
            prefix,
            client,
            runtime,
        }
    }

    /// Convert a path to an S3 key string.
    fn path_to_key(&self, path: &Path) -> String {
        self.prefix.join(path).to_string_lossy().to_string()
    }
}

impl Backend for AmazonS3 {
    fn read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        let buf = self
            .runtime
            .block_on(async {
                let object = self
                    .client
                    .get_object()
                    .bucket(&self.bucket)
                    .key(&key)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;

                // Collect the body into bytes using the new API
                let body = object.body.collect().await.map_err(|e| e.to_string())?;
                Ok::<Vec<u8>, String>(body.into_bytes().to_vec())
            })
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

        Ok(buf)
    }

    fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = super::path_for(hash)?;
        let key = self.path_to_key(&path);

        info!("S3 write: key={}", key);

        let body = ByteStream::from(data.to_vec());
        self.runtime
            .block_on(async {
                self.client
                    .put_object()
                    .bucket(&self.bucket)
                    .key(&key)
                    .body(body)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok::<(), String>(())
            })
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

        Ok(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        let result = self.runtime.block_on(async {
            self.client
                .head_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
        });

        match result {
            Ok(_) => Ok(true),
            Err(err) => {
                // Check if it's a NotFound error
                if let Some(service_err) = err.as_service_error() {
                    if matches!(service_err, HeadObjectError::NotFound(_)) {
                        return Ok(false);
                    }
                }
                Err(Box::new(err) as Box<dyn std::error::Error>)
            }
        }
    }

    fn delete(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        self.runtime
            .block_on(async {
                self.client
                    .delete_object()
                    .bucket(&self.bucket)
                    .key(&key)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok::<(), String>(())
            })
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

        Ok(())
    }

    fn write_to_path(&self, path: &Path, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        info!("S3 write_to_path: key={}", key);

        let body = ByteStream::from(data.to_vec());
        self.runtime
            .block_on(async {
                self.client
                    .put_object()
                    .bucket(&self.bucket)
                    .key(&key)
                    .body(body)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok::<(), String>(())
            })
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

        Ok(())
    }

    fn read_from_path(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        let buf = self
            .runtime
            .block_on(async {
                let object = self
                    .client
                    .get_object()
                    .bucket(&self.bucket)
                    .key(&key)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;

                let body = object.body.collect().await.map_err(|e| e.to_string())?;
                Ok::<Vec<u8>, String>(body.into_bytes().to_vec())
            })
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

        Ok(buf)
    }
}
