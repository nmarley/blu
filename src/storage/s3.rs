//! Amazon S3 storage backend implementation.

use aws_sdk_s3::operation::head_object::HeadObjectError;
use aws_sdk_s3::primitives::ByteStream;
use std::path::{Path, PathBuf};

use crate::error::BluError;
use crate::hash::Hash;

/// Amazon S3 storage backend.
///
/// This backend stores encrypted blob files in an S3 bucket. All I/O
/// is async and driven by the caller's Tokio runtime.
///
/// `Clone` is cheap: `aws_sdk_s3::Client` is `Arc`-backed internally.
#[derive(Clone)]
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
    pub async fn read_data(&self, path: &Path) -> Result<Vec<u8>, BluError> {
        let key = self.path_to_key(path);

        let object = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| BluError::S3Error(e.to_string()))?;

        let body = object
            .body
            .collect()
            .await
            .map_err(|e| BluError::S3Error(e.to_string()))?;
        Ok(body.into_bytes().to_vec())
    }

    /// Read the byte range `[start, end)` (end exclusive) of the object
    /// at the given path.
    ///
    /// HTTP `Range` is inclusive on both ends, so the request uses
    /// `bytes={start}-{end-1}`. S3 clamps the upper bound to the object
    /// size, so a window past EOF returns the available tail. An empty
    /// window (`end <= start`) returns an empty vector without a
    /// request.
    pub async fn read_range(&self, path: &Path, start: u64, end: u64) -> Result<Vec<u8>, BluError> {
        if end <= start {
            return Ok(Vec::new());
        }
        let key = self.path_to_key(path);

        let object = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .range(format!("bytes={}-{}", start, end - 1))
            .send()
            .await
            .map_err(|e| BluError::S3Error(e.to_string()))?;

        let body = object
            .body
            .collect()
            .await
            .map_err(|e| BluError::S3Error(e.to_string()))?;
        Ok(body.into_bytes().to_vec())
    }

    /// Write data to a content-addressed path derived from the hash.
    pub async fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, BluError> {
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
            .map_err(|e| BluError::S3Error(e.to_string()))?;

        Ok(path)
    }

    /// Check if a blob exists at the given path.
    pub async fn exists(&self, path: &Path) -> Result<bool, BluError> {
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
                Err(BluError::S3Error(err.to_string()))
            }
        }
    }

    /// Delete a blob at the given path.
    pub async fn delete(&self, path: &Path) -> Result<(), BluError> {
        let key = self.path_to_key(path);

        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| BluError::S3Error(e.to_string()))?;

        Ok(())
    }

    /// Write data to a known path in the backend (not hash-derived).
    pub async fn write_to_path(&self, path: &Path, data: &[u8]) -> Result<(), BluError> {
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
            .map_err(|e| BluError::S3Error(e.to_string()))?;

        Ok(())
    }

    /// Read data from a known path in the backend (not hash-derived).
    pub async fn read_from_path(&self, path: &Path) -> Result<Vec<u8>, BluError> {
        let key = self.path_to_key(path);

        let object = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| BluError::S3Error(e.to_string()))?;

        let body = object
            .body
            .collect()
            .await
            .map_err(|e| BluError::S3Error(e.to_string()))?;
        Ok(body.into_bytes().to_vec())
    }

    /// List relative paths of content-addressed blob objects under the
    /// backend prefix.
    ///
    /// Uses paginated `ListObjectsV2`. Skips `indexes/` and `keys/`
    /// relative to the backend prefix. Keys are returned without the
    /// configured prefix, matching local relative paths.
    pub async fn list_blob_paths(&self) -> Result<Vec<PathBuf>, BluError> {
        let mut out = Vec::new();
        let list_prefix = {
            let p = self.prefix.to_string_lossy();
            if p.is_empty() {
                String::new()
            } else if p.ends_with('/') {
                p.into_owned()
            } else {
                format!("{}/", p)
            }
        };

        let mut continuation: Option<String> = None;
        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .max_keys(1000);
            if !list_prefix.is_empty() {
                req = req.prefix(&list_prefix);
            }
            if let Some(token) = continuation.take() {
                req = req.continuation_token(token);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| BluError::S3Error(e.to_string()))?;

            for obj in resp.contents() {
                let Some(key) = obj.key() else {
                    continue;
                };
                let rel = if list_prefix.is_empty() {
                    key.to_string()
                } else if let Some(stripped) = key.strip_prefix(&list_prefix) {
                    stripped.to_string()
                } else {
                    continue;
                };
                if rel.is_empty() || rel.ends_with('/') {
                    continue;
                }
                let path = PathBuf::from(&rel);
                if super::is_non_blob_prefix(&path) {
                    continue;
                }
                out.push(path);
            }

            if resp.is_truncated().unwrap_or(false) {
                continuation = resp.next_continuation_token().map(|s| s.to_string());
                if continuation.is_none() {
                    break;
                }
            } else {
                break;
            }
        }

        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::hash::multihash;

    /// Live S3 range-read test. Ignored by default because it needs a
    /// real bucket and credentials. Run with:
    ///
    /// ```sh
    /// BLU_TEST_S3_BUCKET=my-bucket cargo test --  \
    ///     --ignored s3_read_range_live
    /// ```
    ///
    /// Optional: `BLU_TEST_S3_PREFIX`, `AWS_REGION`. Uses the ambient
    /// AWS credential chain (profile, env, or instance role).
    #[tokio::test]
    #[ignore = "requires live S3 bucket and credentials"]
    async fn s3_read_range_live() {
        let bucket =
            std::env::var("BLU_TEST_S3_BUCKET").expect("set BLU_TEST_S3_BUCKET to run this test");
        let prefix = std::env::var("BLU_TEST_S3_PREFIX").ok();
        let region = std::env::var("AWS_REGION").ok();

        let backend = AmazonS3::new(&bucket, prefix.as_deref(), region.as_deref()).await;

        let data: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let hash = Hash::from(multihash(&data).to_bytes());
        let path = backend.write_data(&hash, &data).await.unwrap();

        // Interior window returns exactly the requested bytes.
        let window = backend.read_range(&path, 1000, 2000).await.unwrap();
        assert_eq!(window, &data[1000..2000]);

        // End past EOF clamps to the object tail.
        let tail = backend.read_range(&path, 4000, 1_000_000).await.unwrap();
        assert_eq!(tail, &data[4000..]);

        // Empty window issues no request and returns empty.
        let empty = backend.read_range(&path, 10, 10).await.unwrap();
        assert!(empty.is_empty());

        backend.delete(&path).await.unwrap();
    }
}
