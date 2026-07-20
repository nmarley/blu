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

/// Minimum days AWS allows for Archive Access tiering.
pub const MIN_ARCHIVE_DAYS: u32 = 90;

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
/// `archive_days` adds an optional Archive Access tier before Deep
/// Archive Access; AWS requires it to be strictly less than
/// `deep_archive_days`.
///
/// Output matches the shape expected by
/// `aws s3api put-bucket-intelligent-tiering-configuration
/// --intelligent-tiering-configuration file://...`.
pub fn config_json(
    id: &str,
    prefix: Option<&str>,
    deep_archive_days: u32,
    archive_days: Option<u32>,
) -> Result<String, BluError> {
    if !(MIN_DEEP_ARCHIVE_DAYS..=MAX_ARCHIVE_DAYS).contains(&deep_archive_days) {
        return Err(BluError::InvalidConfig(format!(
            "deep archive days must be between {} and {} (got {})",
            MIN_DEEP_ARCHIVE_DAYS, MAX_ARCHIVE_DAYS, deep_archive_days
        )));
    }
    if let Some(days) = archive_days {
        if !(MIN_ARCHIVE_DAYS..=MAX_ARCHIVE_DAYS).contains(&days) {
            return Err(BluError::InvalidConfig(format!(
                "archive days must be between {} and {} (got {})",
                MIN_ARCHIVE_DAYS, MAX_ARCHIVE_DAYS, days
            )));
        }
        if days >= deep_archive_days {
            return Err(BluError::InvalidConfig(format!(
                "archive days ({}) must be less than deep archive days ({})",
                days, deep_archive_days
            )));
        }
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

    let mut tierings = Vec::with_capacity(2);
    if let Some(days) = archive_days {
        tierings.push(serde_json::json!({
            "Days": days,
            "AccessTier": "ARCHIVE_ACCESS",
        }));
    }
    tierings.push(serde_json::json!({
        "Days": deep_archive_days,
        "AccessTier": "DEEP_ARCHIVE_ACCESS",
    }));

    let doc = serde_json::json!({
        "Id": id,
        "Status": "Enabled",
        "Filter": filter,
        "Tierings": tierings,
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
        let json =
            config_json(DEFAULT_IT_CONFIG_ID, None, DEFAULT_DEEP_ARCHIVE_DAYS, None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["Id"], DEFAULT_IT_CONFIG_ID);
        assert_eq!(v["Status"], "Enabled");
        assert_eq!(v["Filter"]["And"]["Prefix"], "blobs/");
        assert_eq!(v["Filter"]["And"]["Tags"][0]["Key"], TAG_ROLE_KEY);
        assert_eq!(v["Filter"]["And"]["Tags"][0]["Value"], TAG_ROLE_BLOB);
        assert_eq!(v["Tierings"][0]["Days"], 365);
        assert_eq!(v["Tierings"][0]["AccessTier"], "DEEP_ARCHIVE_ACCESS");
        assert_eq!(v["Tierings"].as_array().unwrap().len(), 1);
        assert!(v["Filter"].get("Tag").is_none());
    }

    #[test]
    fn config_json_with_prefix() {
        let json = config_json("blu-media", Some("vaults/photos"), 365, None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["Filter"]["And"]["Prefix"], "vaults/photos/blobs/");
        assert_eq!(v["Filter"]["And"]["Tags"][0]["Value"], TAG_ROLE_BLOB);
        assert!(v["Filter"].get("Tag").is_none());
    }

    #[test]
    fn config_json_rejects_days_below_min() {
        let err = config_json("id", None, 90, None).unwrap_err();
        assert!(err.to_string().contains("between"));
    }

    #[test]
    fn config_json_with_archive_tier() {
        let json = config_json(DEFAULT_IT_CONFIG_ID, None, 365, Some(180)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let tierings = v["Tierings"].as_array().unwrap();
        assert_eq!(tierings.len(), 2);
        assert_eq!(tierings[0]["Days"], 180);
        assert_eq!(tierings[0]["AccessTier"], "ARCHIVE_ACCESS");
        assert_eq!(tierings[1]["Days"], 365);
        assert_eq!(tierings[1]["AccessTier"], "DEEP_ARCHIVE_ACCESS");
    }

    #[test]
    fn config_json_rejects_archive_days_out_of_range() {
        let err = config_json("id", None, 365, Some(89)).unwrap_err();
        assert!(err.to_string().contains("between"));
        let err = config_json("id", None, 365, Some(731)).unwrap_err();
        assert!(err.to_string().contains("between"));
    }

    #[test]
    fn config_json_rejects_archive_not_less_than_deep() {
        let err = config_json("id", None, 365, Some(365)).unwrap_err();
        assert!(err.to_string().contains("less than"));
        let err = config_json("id", None, 365, Some(400)).unwrap_err();
        assert!(err.to_string().contains("less than"));
        let err = config_json("id", None, 180, Some(180)).unwrap_err();
        assert!(err.to_string().contains("less than"));
    }

    #[test]
    fn apply_command_includes_bucket_and_id() {
        let cmd = apply_command_hint("my-bucket", "blu-blobs-deep-archive", Some("us-west-2"));
        assert!(cmd.contains("--bucket my-bucket"));
        assert!(cmd.contains("--id blu-blobs-deep-archive"));
        assert!(cmd.contains("--region us-west-2"));
    }
}
