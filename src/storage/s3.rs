//! Amazon S3 storage backend implementation.

use aws_sdk_s3::operation::{
    delete_object::DeleteObjectError,
    get_object::{GetObjectError, GetObjectOutput},
    head_object::HeadObjectError,
    put_object::{PutObjectError, PutObjectOutput},
};
use aws_sdk_s3::{error::SdkError, primitives::ByteStream};
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio_stream::StreamExt;

use crate::hash::Hash;

use super::StorageBackend;

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
            let mut config_loader = aws_config::from_env();

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

    /// Check if an object exists in S3.
    pub fn exists(&self, path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
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
            Err(SdkError::ServiceError(err)) => {
                // Check if it's a NotFound error
                if matches!(err.err(), HeadObjectError::NotFound(_)) {
                    Ok(false)
                } else {
                    Err(Box::new(err.into_err()) as Box<dyn std::error::Error>)
                }
            }
            Err(e) => Err(Box::new(e) as Box<dyn std::error::Error>),
        }
    }

    /// Delete an object from S3.
    pub fn delete(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        self.runtime.block_on(async {
            let _output = self
                .client
                .delete_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await?;
            Ok::<(), SdkError<DeleteObjectError, HttpResponse>>(())
        })?;

        Ok(())
    }
}

impl StorageBackend for AmazonS3 {
    fn read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        let mut object = self.runtime.block_on(async {
            let object = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await?;
            Ok::<GetObjectOutput, SdkError<GetObjectError, HttpResponse>>(object)
        })?;

        let buf = self.runtime.block_on(async {
            let mut buf: Vec<u8> = vec![];
            while let Some(bytes) = object.body.try_next().await? {
                buf.extend_from_slice(&bytes);
            }
            Ok::<Vec<u8>, Box<dyn std::error::Error>>(buf)
        })?;

        Ok(buf)
    }

    fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = super::path_for(hash)?;
        let key = self.path_to_key(&path);

        info!("S3 write: key={}", key);

        let body = ByteStream::from(data.to_vec());
        self.runtime.block_on(async {
            let _put_obj_output = self
                .client
                .put_object()
                .bucket(&self.bucket)
                .key(&key)
                .body(body)
                .send()
                .await?;
            Ok::<PutObjectOutput, SdkError<PutObjectError, HttpResponse>>(_put_obj_output)
        })?;

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
            Err(SdkError::ServiceError(err)) => {
                if matches!(err.err(), HeadObjectError::NotFound(_)) {
                    Ok(false)
                } else {
                    Err(Box::new(err.into_err()) as Box<dyn std::error::Error>)
                }
            }
            Err(e) => Err(Box::new(e) as Box<dyn std::error::Error>),
        }
    }

    fn delete(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let key = self.path_to_key(path);

        self.runtime.block_on(async {
            let _output = self
                .client
                .delete_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await?;
            Ok::<(), SdkError<DeleteObjectError, HttpResponse>>(())
        })?;

        Ok(())
    }
}
