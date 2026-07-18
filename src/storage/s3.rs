//! Amazon S3 storage backend implementation.

use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::operation::head_object::HeadObjectError;
use aws_sdk_s3::operation::restore_object::RestoreObjectError;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{GlacierJobParameters, RestoreRequest, StorageClass, Tier};
use std::path::{Path, PathBuf};

use super::{
    ObjectAvailability, ObjectStat, RestoreOptions, RestoreTier, TAG_ROLE_BLOB, TAG_ROLE_CATALOG,
    TAG_ROLE_KEY,
};
use crate::error::BluError;
use crate::hash::Hash;

/// Amazon S3 storage backend.
///
/// This backend stores encrypted blob files in an S3 bucket. All I/O
/// is async and driven by the caller's Tokio runtime.
///
/// Blob puts use `INTELLIGENT_TIERING` and tag `blu-role=blob`. Catalog
/// puts (`indexes/`, `keys/`) use `STANDARD` and tag `blu-role=catalog`.
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
            .map_err(|e| map_get_error(path, e))?;

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
            .map_err(|e| map_get_error(path, e))?;

        let body = object
            .body
            .collect()
            .await
            .map_err(|e| BluError::S3Error(e.to_string()))?;
        Ok(body.into_bytes().to_vec())
    }

    /// Write data to a content-addressed path derived from the hash.
    ///
    /// Puts as `INTELLIGENT_TIERING` with `blu-role=blob` so bucket
    /// Intelligent-Tiering archive configs can filter blob objects.
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
            .storage_class(StorageClass::IntelligentTiering)
            .tagging(role_tagging_query(TAG_ROLE_BLOB))
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
    ///
    /// Catalog paths (`indexes/`, `keys/`) are put as `STANDARD` with
    /// `blu-role=catalog`. Other known paths default to the same catalog
    /// policy so non-blob objects stay hot.
    pub async fn write_to_path(&self, path: &Path, data: &[u8]) -> Result<(), BluError> {
        let key = self.path_to_key(path);

        info!("S3 write_to_path: key={}", key);

        let body = ByteStream::from(data.to_vec());
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(body)
            .storage_class(StorageClass::Standard)
            .tagging(role_tagging_query(TAG_ROLE_CATALOG))
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
            .map_err(|e| map_get_error(path, e))?;

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
    /// Uses paginated `ListObjectsV2` scoped to the `blobs/` prefix, so
    /// catalog material (`indexes/`, `keys/`) is never listed. Keys are
    /// returned without the configured prefix, matching local relative
    /// paths.
    pub async fn list_blob_paths(&self) -> Result<Vec<PathBuf>, BluError> {
        let mut out = Vec::new();
        let vault_prefix = {
            let p = self.prefix.to_string_lossy();
            if p.is_empty() {
                String::new()
            } else if p.ends_with('/') {
                p.into_owned()
            } else {
                format!("{}/", p)
            }
        };
        let list_prefix = format!("{}{}/", vault_prefix, super::BLOB_PREFIX);

        let mut continuation: Option<String> = None;
        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&list_prefix)
                .max_keys(1000);
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
                // Strip the vault prefix (not the blobs/ component) so
                // relative paths match `path_for` output.
                let rel = if vault_prefix.is_empty() {
                    key.to_string()
                } else if let Some(stripped) = key.strip_prefix(&vault_prefix) {
                    stripped.to_string()
                } else {
                    continue;
                };
                if rel.is_empty() || rel.ends_with('/') {
                    continue;
                }
                out.push(PathBuf::from(&rel));
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

    /// HeadObject probe for storage class, archive status, and restore state.
    pub async fn stat_object(&self, path: &Path) -> Result<ObjectStat, BluError> {
        let key = self.path_to_key(path);

        let resp = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|err| {
                if let Some(service_err) = err.as_service_error() {
                    if matches!(service_err, HeadObjectError::NotFound(_)) {
                        return BluError::StorageFileNotFound {
                            path: path.to_path_buf(),
                        };
                    }
                }
                BluError::S3Error(err.to_string())
            })?;

        let storage_class = resp.storage_class().map(|c| c.as_str().to_string());
        let archive_status = resp.archive_status().map(|s| s.as_str().to_string());
        let restore_header = resp.restore().map(|s| s.to_string());
        let content_length = resp.content_length().map(|n| n as u64);

        let availability = classify_availability(
            storage_class.as_deref(),
            archive_status.as_deref(),
            restore_header.as_deref(),
        );

        Ok(ObjectStat {
            path: path.to_path_buf(),
            storage_class,
            archive_status,
            availability,
            restore_header,
            content_length,
        })
    }

    /// Initiate RestoreObject for an archived object.
    ///
    /// `prior` is an optional earlier HeadObject probe (e.g. from a
    /// thaw classification); when provided it is trusted and no new
    /// HEAD is issued. Pass `None` to re-probe current state first.
    ///
    /// Intelligent-Tiering archive tiers use `Tier` only (no `Days`).
    /// Classic Glacier / Deep Archive storage classes use `Days` plus
    /// `GlacierJobParameters`. Already-in-progress and already-hot
    /// errors are treated as success (idempotent).
    pub async fn restore_object(
        &self,
        path: &Path,
        prior: Option<&ObjectStat>,
        opts: &RestoreOptions,
    ) -> Result<(), BluError> {
        let key = self.path_to_key(path);
        let owned;
        let stat = match prior {
            Some(s) => s,
            None => {
                owned = self.stat_object(path).await?;
                &owned
            }
        };

        match stat.availability {
            ObjectAvailability::Available | ObjectAvailability::Restored { .. } => {
                return Ok(());
            }
            ObjectAvailability::Restoring => return Ok(()),
            ObjectAvailability::Archived => {}
        }

        let tier = match opts.tier {
            RestoreTier::Bulk => Tier::Bulk,
            RestoreTier::Standard => Tier::Standard,
        };

        let restore_request = build_restore_request(stat.storage_class.as_deref(), opts.days, tier);

        let result = self
            .client
            .restore_object()
            .bucket(&self.bucket)
            .key(&key)
            .restore_request(restore_request)
            .send()
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => map_restore_error(err),
        }
    }
}

fn role_tagging_query(role: &str) -> String {
    format!("{TAG_ROLE_KEY}={role}")
}

fn map_get_error(path: &Path, err: SdkError<GetObjectError>) -> BluError {
    if let Some(GetObjectError::InvalidObjectState(state)) = err.as_service_error() {
        return BluError::ObjectArchived {
            path: path.to_path_buf(),
            storage_class: state.storage_class().map(|c| c.as_str().to_string()),
            access_tier: state.access_tier().map(|t| t.as_str().to_string()),
        };
    }
    BluError::S3Error(err.to_string())
}

fn map_restore_error(err: SdkError<RestoreObjectError>) -> Result<(), BluError> {
    if let Some(service_err) = err.as_service_error() {
        if matches!(
            service_err,
            RestoreObjectError::ObjectAlreadyInActiveTierError(_)
        ) {
            return Ok(());
        }
        if service_err.code() == Some("RestoreAlreadyInProgress") {
            return Ok(());
        }
    }
    if err.code() == Some("RestoreAlreadyInProgress") {
        return Ok(());
    }
    Err(BluError::S3Error(err.to_string()))
}

fn build_restore_request(storage_class: Option<&str>, days: u32, tier: Tier) -> RestoreRequest {
    let days_i32 = i32::try_from(days).unwrap_or(i32::MAX);
    match storage_class {
        Some("INTELLIGENT_TIERING") => RestoreRequest::builder().tier(tier).build(),
        // GLACIER_IR is instant retrieval: it classifies as Available
        // and never reaches restore request building.
        Some("GLACIER") | Some("DEEP_ARCHIVE") => {
            let glacier = GlacierJobParameters::builder()
                .tier(tier)
                .build()
                .expect("tier required");
            RestoreRequest::builder()
                .days(days_i32)
                .glacier_job_parameters(glacier)
                .build()
        }
        // Unknown or missing class: send both forms S3 accepts for archive
        // restores (days + top-level tier). Prefer glacier-style days for
        // classic archive classes; IT ignores days when tier is set.
        _ => RestoreRequest::builder()
            .days(days_i32)
            .tier(tier.clone())
            .glacier_job_parameters(
                GlacierJobParameters::builder()
                    .tier(tier)
                    .build()
                    .expect("tier required"),
            )
            .build(),
    }
}

/// Parse the S3 `x-amz-restore` header.
///
/// Examples:
/// - `ongoing-request="true"`
/// - `ongoing-request="false", expiry-date="Fri, 01 Jan 2027 00:00:00 GMT"`
fn parse_restore_header(header: &str) -> (bool, Option<String>) {
    let ongoing = header.contains("ongoing-request=\"true\"");
    let expiry_hint = header
        .split("expiry-date=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .map(|s| s.to_string());
    (ongoing, expiry_hint)
}

/// Classify GET availability from HeadObject fields.
fn classify_availability(
    storage_class: Option<&str>,
    archive_status: Option<&str>,
    restore_header: Option<&str>,
) -> ObjectAvailability {
    let (ongoing, expiry_hint) = match restore_header {
        Some(h) => parse_restore_header(h),
        None => (false, None),
    };

    if ongoing {
        return ObjectAvailability::Restoring;
    }

    // Temporary restore copy present (classic Glacier / Deep Archive).
    if restore_header.is_some() && !ongoing {
        return ObjectAvailability::Restored { expiry_hint };
    }

    match storage_class {
        Some("GLACIER") | Some("DEEP_ARCHIVE") => ObjectAvailability::Archived,
        Some("INTELLIGENT_TIERING") => match archive_status {
            Some("ARCHIVE_ACCESS") | Some("DEEP_ARCHIVE_ACCESS") => ObjectAvailability::Archived,
            _ => ObjectAvailability::Available,
        },
        _ => ObjectAvailability::Available,
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::hash::multihash;

    #[test]
    fn role_tagging_query_blob_and_catalog() {
        assert_eq!(role_tagging_query(TAG_ROLE_BLOB), "blu-role=blob");
        assert_eq!(role_tagging_query(TAG_ROLE_CATALOG), "blu-role=catalog");
    }

    #[test]
    fn parse_restore_header_ongoing() {
        let (ongoing, expiry) = parse_restore_header("ongoing-request=\"true\"");
        assert!(ongoing);
        assert!(expiry.is_none());
    }

    #[test]
    fn parse_restore_header_restored() {
        let (ongoing, expiry) = parse_restore_header(
            "ongoing-request=\"false\", expiry-date=\"Fri, 01 Jan 2027 00:00:00 GMT\"",
        );
        assert!(!ongoing);
        assert_eq!(expiry.as_deref(), Some("Fri, 01 Jan 2027 00:00:00 GMT"));
    }

    #[test]
    fn classify_intelligent_tiering_deep_archive() {
        let avail = classify_availability(
            Some("INTELLIGENT_TIERING"),
            Some("DEEP_ARCHIVE_ACCESS"),
            None,
        );
        assert_eq!(avail, ObjectAvailability::Archived);
    }

    #[test]
    fn classify_intelligent_tiering_hot() {
        let avail = classify_availability(Some("INTELLIGENT_TIERING"), None, None);
        assert_eq!(avail, ObjectAvailability::Available);
    }

    #[test]
    fn classify_glacier_restoring() {
        let avail =
            classify_availability(Some("DEEP_ARCHIVE"), None, Some("ongoing-request=\"true\""));
        assert_eq!(avail, ObjectAvailability::Restoring);
    }

    #[test]
    fn classify_glacier_restored() {
        let avail = classify_availability(
            Some("DEEP_ARCHIVE"),
            None,
            Some("ongoing-request=\"false\", expiry-date=\"Fri, 01 Jan 2027 00:00:00 GMT\""),
        );
        match avail {
            ObjectAvailability::Restored { expiry_hint } => {
                assert_eq!(
                    expiry_hint.as_deref(),
                    Some("Fri, 01 Jan 2027 00:00:00 GMT")
                );
            }
            other => panic!("expected Restored, got {other:?}"),
        }
    }

    #[test]
    fn classify_standard_available() {
        let avail = classify_availability(Some("STANDARD"), None, None);
        assert_eq!(avail, ObjectAvailability::Available);
    }

    #[test]
    fn build_restore_request_it_has_tier_no_days() {
        let req = build_restore_request(Some("INTELLIGENT_TIERING"), 14, Tier::Bulk);
        assert!(req.days().is_none());
        assert_eq!(req.tier(), Some(&Tier::Bulk));
        assert!(req.glacier_job_parameters().is_none());
    }

    #[test]
    fn build_restore_request_deep_archive_has_days() {
        let req = build_restore_request(Some("DEEP_ARCHIVE"), 14, Tier::Standard);
        assert_eq!(req.days(), Some(14));
        assert!(req.glacier_job_parameters().is_some());
        assert_eq!(
            req.glacier_job_parameters().unwrap().tier(),
            &Tier::Standard
        );
    }

    /// Build a backend for live S3 tests from environment variables.
    ///
    /// `BLU_TEST_S3_BUCKET` is required; `BLU_TEST_S3_PREFIX` and
    /// `AWS_REGION` are optional. Uses the ambient AWS credential
    /// chain (profile, env, or instance role).
    async fn live_backend() -> AmazonS3 {
        let bucket =
            std::env::var("BLU_TEST_S3_BUCKET").expect("set BLU_TEST_S3_BUCKET to run this test");
        let prefix = std::env::var("BLU_TEST_S3_PREFIX").ok();
        let region = std::env::var("AWS_REGION").ok();
        AmazonS3::new(&bucket, prefix.as_deref(), region.as_deref()).await
    }

    /// Fresh key under `blobs/live-test/` so re-runs never collide.
    fn live_test_path(label: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_secs();
        PathBuf::from(format!("blobs/live-test/{label}-{ts}"))
    }

    /// Live S3 range-read test. Ignored by default because it needs a
    /// real bucket and credentials. Run with:
    ///
    /// ```sh
    /// BLU_TEST_S3_BUCKET=my-bucket cargo test --  \
    ///     --ignored s3_read_range_live
    /// ```
    #[tokio::test]
    #[ignore = "requires live S3 bucket and credentials"]
    async fn s3_read_range_live() {
        let backend = live_backend().await;

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

    /// Live verification of the Intelligent-Tiering restore path.
    ///
    /// Confirms the put policy landed (IT storage class, `blu-role=blob`
    /// tag, hot availability) and that the IT restore request shape
    /// (Tier only, no Days) is accepted by S3: a hot IT object cannot
    /// be restored, so S3 must reject with exactly
    /// `ObjectAlreadyInActiveTierError`, which proves the request
    /// serialized and was understood. Finishes in seconds.
    ///
    /// Runbook:
    ///
    /// ```sh
    /// BLU_TEST_S3_BUCKET=my-bucket cargo test -- \
    ///     --ignored s3_restore_request_shape_it_live --nocapture
    /// ```
    ///
    /// Creates and deletes one small blob object. Record results in
    /// `docs/design/S3_COLD_STORAGE_DESIGN.md`.
    #[tokio::test]
    #[ignore = "requires live S3 bucket and credentials"]
    async fn s3_restore_request_shape_it_live() {
        let backend = live_backend().await;

        // write_data puts INTELLIGENT_TIERING + blu-role=blob.
        let data = b"blu live IT restore-shape probe";
        let hash = Hash::from(multihash(data).to_bytes());
        let path = backend.write_data(&hash, data).await.unwrap();

        // Confirm the put policy landed: IT class, hot, blob tag.
        let stat = backend.stat_object(&path).await.unwrap();
        assert_eq!(stat.storage_class.as_deref(), Some("INTELLIGENT_TIERING"));
        assert_eq!(stat.availability, ObjectAvailability::Available);

        let key = backend.path_to_key(&path);
        let tags = backend
            .client
            .get_object_tagging()
            .bucket(&backend.bucket)
            .key(&key)
            .send()
            .await
            .unwrap();
        assert!(tags
            .tag_set()
            .iter()
            .any(|t| t.key() == TAG_ROLE_KEY && t.value() == TAG_ROLE_BLOB));

        // Raw RestoreObject with the IT shape (Tier only, no Days).
        // A hot IT object cannot be restored; S3 must reject with
        // ObjectAlreadyInActiveTierError, proving the request
        // serialized correctly and was understood.
        let req = build_restore_request(Some("INTELLIGENT_TIERING"), 14, Tier::Bulk);
        let err = backend
            .client
            .restore_object()
            .bucket(&backend.bucket)
            .key(&key)
            .restore_request(req)
            .send()
            .await
            .expect_err("hot IT object must reject restore");
        match err.as_service_error() {
            Some(RestoreObjectError::ObjectAlreadyInActiveTierError(_)) => {}
            other => panic!("expected ObjectAlreadyInActiveTierError, got {other:?}"),
        }

        // The public API short-circuits hot objects without sending a
        // restore, both with a fresh HEAD and with a prior stat.
        backend
            .restore_object(&path, None, &RestoreOptions::default())
            .await
            .unwrap();
        backend
            .restore_object(&path, Some(&stat), &RestoreOptions::default())
            .await
            .unwrap();

        backend.delete(&path).await.unwrap();
    }

    /// Live verification of a real Bulk restore from the GLACIER
    /// storage class, from initiation through a successful GET.
    ///
    /// This is a multi-hour test: Bulk retrieval from S3 Glacier
    /// Flexible Retrieval typically completes in 5-12 hours. Run it,
    /// walk away, check back.
    ///
    /// Runbook:
    ///
    /// ```sh
    /// BLU_TEST_S3_BUCKET=my-bucket cargo test -- \
    ///     --ignored s3_restore_glacier_bulk_live --nocapture
    /// ```
    ///
    /// Optional: `BLU_TEST_RESTORE_TIMEOUT_HOURS` (default 26).
    /// Creates one small GLACIER object per run and deletes it
    /// afterwards (early-deletion fee on a few KB is a fraction of a
    /// cent). Record the observed duration and any error shapes in
    /// `docs/design/S3_COLD_STORAGE_DESIGN.md`.
    #[tokio::test]
    #[ignore = "requires live S3 bucket, credentials, and multi-hour wait"]
    async fn s3_restore_glacier_bulk_live() {
        let backend = live_backend().await;
        let path = live_test_path("glacier-bulk-restore");
        let key = backend.path_to_key(&path);

        let data = b"blu live glacier bulk restore probe".to_vec();
        backend
            .client
            .put_object()
            .bucket(&backend.bucket)
            .key(&key)
            .body(ByteStream::from(data.clone()))
            .storage_class(StorageClass::Glacier)
            .send()
            .await
            .unwrap();

        // A fresh GLACIER object is archived and not GET-able.
        let stat = backend.stat_object(&path).await.unwrap();
        assert_eq!(stat.availability, ObjectAvailability::Archived);

        // Initiate a Bulk restore; the temporary copy lives for 1 day.
        let opts = RestoreOptions {
            days: 1,
            tier: RestoreTier::Bulk,
        };
        backend
            .restore_object(&path, Some(&stat), &opts)
            .await
            .unwrap();

        // The restore is now in flight, and re-firing is a no-op.
        let stat = backend.stat_object(&path).await.unwrap();
        assert_eq!(stat.availability, ObjectAvailability::Restoring);
        backend
            .restore_object(&path, Some(&stat), &opts)
            .await
            .unwrap();

        // Poll until the temporary copy exists (multi-hour for Bulk).
        let timeout_hours: u64 = std::env::var("BLU_TEST_RESTORE_TIMEOUT_HOURS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(26);
        let started = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(timeout_hours * 3600);
        let final_stat = loop {
            let stat = backend.stat_object(&path).await.unwrap();
            match stat.availability {
                ObjectAvailability::Restored { .. } | ObjectAvailability::Available => break stat,
                ObjectAvailability::Restoring => {
                    assert!(
                        started.elapsed() < timeout,
                        "restore did not complete within {}h",
                        timeout_hours
                    );
                    eprintln!("still restoring (elapsed {:?})", started.elapsed());
                    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                }
                ObjectAvailability::Archived => panic!("restore was never initiated"),
            }
        };
        eprintln!(
            "restore completed in {:?}; x-amz-restore: {:?}",
            started.elapsed(),
            final_stat.restore_header
        );

        // GET works against the temporary copy and returns the bytes.
        let got = backend.read_data(&path).await.unwrap();
        assert_eq!(got, data);

        backend.delete(&path).await.unwrap();
    }
}
