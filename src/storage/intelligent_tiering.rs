//! Recommended S3 Intelligent-Tiering archive configuration for blu vaults.
//!
//! Prints operator-owned JSON for
//! `PutBucketIntelligentTieringConfiguration`. Blu never applies this
//! automatically; the operator applies it once per bucket.

use crate::error::BluError;
use crate::storage::{BLOB_PREFIX, TAG_ROLE_BLOB, TAG_ROLE_KEY};

/// Default configuration id for blu blob Deep Archive Access.
pub const DEFAULT_IT_CONFIG_ID: &str = "blu-blobs-deep-archive";

/// Default consecutive days of no access before Deep Archive Access.
pub const DEFAULT_DEEP_ARCHIVE_DAYS: u32 = 365;

/// Minimum days AWS allows for Deep Archive Access tiering.
pub const MIN_DEEP_ARCHIVE_DAYS: u32 = 180;

/// Maximum days AWS allows for archive access tiering.
pub const MAX_ARCHIVE_DAYS: u32 = 730;

/// Build the Intelligent-Tiering configuration JSON for blu blob objects.
///
/// Filter is AND of the `blobs/` key prefix (scoped by the vault prefix
/// when set) and tag `blu-role=blob`, so catalog objects
/// (`indexes/`, `keys/`, `blu-role=catalog`) are never archived.
///
/// Output matches the shape expected by
/// `aws s3api put-bucket-intelligent-tiering-configuration
/// --intelligent-tiering-configuration file://...`.
pub fn config_json(
    id: &str,
    prefix: Option<&str>,
    deep_archive_days: u32,
) -> Result<String, BluError> {
    if !(MIN_DEEP_ARCHIVE_DAYS..=MAX_ARCHIVE_DAYS).contains(&deep_archive_days) {
        return Err(BluError::InvalidConfig(format!(
            "deep archive days must be between {} and {} (got {})",
            MIN_DEEP_ARCHIVE_DAYS, MAX_ARCHIVE_DAYS, deep_archive_days
        )));
    }
    if id.is_empty() {
        return Err(BluError::InvalidConfig(
            "intelligent-tiering configuration id must not be empty".into(),
        ));
    }

    let vault_prefix = normalize_prefix(prefix).unwrap_or_default();
    let blob_prefix = format!("{}{}/", vault_prefix, BLOB_PREFIX);
    let filter = serde_json::json!({
        "And": {
            "Prefix": blob_prefix,
            "Tags": [{
                "Key": TAG_ROLE_KEY,
                "Value": TAG_ROLE_BLOB,
            }]
        }
    });

    let doc = serde_json::json!({
        "Id": id,
        "Status": "Enabled",
        "Filter": filter,
        "Tierings": [{
            "Days": deep_archive_days,
            "AccessTier": "DEEP_ARCHIVE_ACCESS",
        }],
    });

    serde_json::to_string_pretty(&doc)
        .map_err(|e| BluError::SerializationError(format!("intelligent-tiering json: {}", e)))
}

/// Suggested `aws s3api` apply command for a bucket (operator-owned).
pub fn apply_command_hint(bucket: &str, id: &str, region: Option<&str>) -> String {
    let region_flag = match region {
        Some(r) if !r.is_empty() => format!(" --region {}", r),
        _ => String::new(),
    };
    format!(
        "aws s3api put-bucket-intelligent-tiering-configuration \
--bucket {bucket} --id {id}{region_flag} \
--intelligent-tiering-configuration file://blu-it-config.json"
    )
}

fn normalize_prefix(prefix: Option<&str>) -> Option<String> {
    let p = prefix?.trim();
    if p.is_empty() {
        return None;
    }
    if p.ends_with('/') {
        Some(p.to_string())
    } else {
        Some(format!("{}/", p))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn config_json_blob_prefix_and_tag() {
        let json = config_json(DEFAULT_IT_CONFIG_ID, None, DEFAULT_DEEP_ARCHIVE_DAYS).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["Id"], DEFAULT_IT_CONFIG_ID);
        assert_eq!(v["Status"], "Enabled");
        assert_eq!(v["Filter"]["And"]["Prefix"], "blobs/");
        assert_eq!(v["Filter"]["And"]["Tags"][0]["Key"], TAG_ROLE_KEY);
        assert_eq!(v["Filter"]["And"]["Tags"][0]["Value"], TAG_ROLE_BLOB);
        assert_eq!(v["Tierings"][0]["Days"], 365);
        assert_eq!(v["Tierings"][0]["AccessTier"], "DEEP_ARCHIVE_ACCESS");
        assert!(v["Filter"].get("Tag").is_none());
    }

    #[test]
    fn config_json_with_prefix() {
        let json = config_json("blu-media", Some("vaults/photos"), 365).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["Filter"]["And"]["Prefix"], "vaults/photos/blobs/");
        assert_eq!(v["Filter"]["And"]["Tags"][0]["Value"], TAG_ROLE_BLOB);
        assert!(v["Filter"].get("Tag").is_none());
    }

    #[test]
    fn config_json_rejects_days_below_min() {
        let err = config_json("id", None, 90).unwrap_err();
        assert!(err.to_string().contains("between"));
    }

    #[test]
    fn apply_command_includes_bucket_and_id() {
        let cmd = apply_command_hint("my-bucket", "blu-blobs-deep-archive", Some("us-west-2"));
        assert!(cmd.contains("--bucket my-bucket"));
        assert!(cmd.contains("--id blu-blobs-deep-archive"));
        assert!(cmd.contains("--region us-west-2"));
    }
}
