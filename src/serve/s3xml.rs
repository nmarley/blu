//! Hand-rolled S3 XML serialization for `blu serve`.
//!
//! The S3 API uses a small, fixed set of XML schemas. Rather than pull
//! in a full XML serialization crate and fight its type system for
//! quirks like ETag literal double-quotes and optional-field omission,
//! we hand-roll the XML via `write!` into a `String`. The schemas are
//! stable and small.
//!
//! All user-provided strings (paths, prefixes, delimiters) are XML-
//! escaped via `xml_escape` before emission.

use std::collections::BTreeSet;

use axum::http::StatusCode;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use chrono::NaiveDateTime;
#[cfg(test)]
use chrono::{TimeZone, Utc};

use crate::hash::Hash;
use crate::serve::redb_store::RedbStore;

/// S3 XML namespace used in all responses.
const S3_NS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// Escape XML special characters in a string.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Format a `NaiveDateTime` as an S3 `LastModified` timestamp:
/// `YYYY-MM-DDTHH:MM:SS.000Z` (ISO 8601, millisecond precision, UTC).
fn format_last_modified(dt: &NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S.000Z").to_string()
}

/// Create a `NaiveDateTime` from a Unix timestamp (test helper).
#[cfg(test)]
fn from_epoch(secs: i64) -> NaiveDateTime {
    Utc.timestamp_opt(secs, 0).unwrap().naive_utc()
}

/// Encode a continuation token. The cursor is the last-processed key
/// (or the next-prefix for a common-prefix group). We base64-encode it
/// to make it opaque, matching S3's behavior of returning obfuscated
/// tokens.
pub(crate) fn encode_continuation_token(cursor: &str) -> String {
    BASE64.encode(cursor.as_bytes())
}

/// Decode a continuation token back into the resume key.
pub(crate) fn decode_continuation_token(token: &str) -> Option<String> {
    BASE64
        .decode(token.as_bytes())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

/// Group path entries into `Contents` and `CommonPrefixes` based on a
/// delimiter.
///
/// If `delimiter` is `None`, all entries go into `Contents`. If a
/// delimiter is set, any path that contains the delimiter after the
/// prefix is grouped into a `CommonPrefix` (the prefix plus everything
/// up to and including the first delimiter after the prefix). Both
/// `Contents` and `CommonPrefixes` count against `MaxKeys`.
///
/// Returns `(contents, common_prefixes, next_cursor)`. The
/// `next_cursor` is the resume key for the next page: if the last
/// consumed entry was a `Contents` entry, it is that entry's key; if
/// the last consumed entry was a `CommonPrefix`, it is the
/// `next_prefix` of that common prefix (to skip all keys under it on
/// the next page).
pub(crate) fn group_by_delimiter(
    entries: &[(String, Hash)],
    prefix: &str,
    delimiter: Option<&str>,
) -> (Vec<(String, Hash)>, BTreeSet<String>, Option<String>) {
    let mut contents = Vec::new();
    let mut common_prefixes: BTreeSet<String> = BTreeSet::new();
    let mut next_cursor: Option<String> = None;

    let delim = match delimiter {
        Some(d) if !d.is_empty() => d,
        _ => {
            // No delimiter: all entries are Contents.
            for (path, _hash) in entries {
                next_cursor = Some(path.clone());
            }
            return (entries.to_vec(), common_prefixes, next_cursor);
        }
    };

    for (path, hash) in entries {
        let suffix = &path[prefix.len()..];
        if let Some(pos) = suffix.find(delim) {
            let common_prefix = format!("{}{}", prefix, &suffix[..=pos + delim.len() - 1]);
            common_prefixes.insert(common_prefix.clone());
            next_cursor = Some(next_prefix_str(&common_prefix));
        } else {
            contents.push((path.clone(), hash.clone()));
            next_cursor = Some(path.clone());
        }
    }

    (contents, common_prefixes, next_cursor)
}

/// Compute the next lexicographic prefix for a string, used to skip
/// all keys under a common prefix on the next page. Delegates to the
/// same algorithm as `redb_store::next_prefix`.
fn next_prefix_str(s: &str) -> String {
    let mut bytes = s.as_bytes().to_vec();
    while let Some(last) = bytes.last_mut() {
        if *last == 0xFF {
            bytes.pop();
            continue;
        }
        *last += 1;
        if std::str::from_utf8(&bytes).is_ok() {
            return String::from_utf8(bytes)
                .expect("checked valid UTF-8 before constructing String");
        }
        bytes.pop();
    }
    // If there is no successor (all bytes are 0xFF or produce invalid
    // UTF-8), return the original string. This means the next page
    // will not find any more keys under this prefix, which is correct
    // because the current page consumed everything.
    s.to_string()
}

/// Build the `ListBucketResult` XML response for `ListObjectsV2`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn list_bucket_result(
    bucket_name: &str,
    prefix: &str,
    delimiter: Option<&str>,
    max_keys: usize,
    continuation_token: &Option<String>,
    start_after: &Option<String>,
    echo_start_after: bool,
    is_truncated: bool,
    next_continuation_token: Option<&str>,
    contents: &[(String, Hash)],
    common_prefixes: &BTreeSet<String>,
    index_updated_at: &NaiveDateTime,
    redb: &RedbStore,
) -> String {
    let last_modified = format_last_modified(index_updated_at);
    let key_count = contents.len() + common_prefixes.len();

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!("<ListBucketResult xmlns=\"{}\">\n", S3_NS));
    xml.push_str(&format!("  <Name>{}</Name>\n", xml_escape(bucket_name)));
    xml.push_str(&format!("  <Prefix>{}</Prefix>\n", xml_escape(prefix)));

    if echo_start_after {
        if let Some(sa) = start_after {
            xml.push_str(&format!("  <StartAfter>{}</StartAfter>\n", xml_escape(sa)));
        }
    }

    if let Some(token) = continuation_token {
        xml.push_str(&format!(
            "  <ContinuationToken>{}</ContinuationToken>\n",
            xml_escape(token)
        ));
    }

    xml.push_str(&format!("  <KeyCount>{}</KeyCount>\n", key_count));
    xml.push_str(&format!("  <MaxKeys>{}</MaxKeys>\n", max_keys));

    if let Some(d) = delimiter {
        xml.push_str(&format!("  <Delimiter>{}</Delimiter>\n", xml_escape(d)));
    }

    xml.push_str(&format!("  <IsTruncated>{}</IsTruncated>\n", is_truncated));

    if let Some(token) = next_continuation_token {
        xml.push_str(&format!(
            "  <NextContinuationToken>{}</NextContinuationToken>\n",
            xml_escape(token)
        ));
    }

    for (path, hash) in contents {
        let size = redb
            .get_fileref(hash)
            .ok()
            .flatten()
            .map(|fr| fr.total_size())
            .unwrap_or(0);

        xml.push_str("  <Contents>\n");
        xml.push_str(&format!("    <Key>{}</Key>\n", xml_escape(path)));
        xml.push_str(&format!(
            "    <LastModified>{}</LastModified>\n",
            last_modified
        ));
        // ETag is the file hash wrapped in literal double quotes. S3
        // does not XML-escape the quotes in ETag values.
        xml.push_str(&format!("    <ETag>\"{}\"</ETag>\n", hash));
        xml.push_str(&format!("    <Size>{}</Size>\n", size));
        xml.push_str("    <StorageClass>STANDARD</StorageClass>\n");
        xml.push_str("  </Contents>\n");
    }

    for cp in common_prefixes {
        xml.push_str("  <CommonPrefixes>\n");
        xml.push_str(&format!("    <Prefix>{}</Prefix>\n", xml_escape(cp)));
        xml.push_str("  </CommonPrefixes>\n");
    }

    xml.push_str("</ListBucketResult>\n");
    xml
}

/// Build the `ListAllMyBucketsResult` XML response for `ListBuckets`.
pub(crate) fn list_all_my_buckets(bucket_name: &str, creation_date: &str) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!("<ListAllMyBucketsResult xmlns=\"{}\">\n", S3_NS));
    xml.push_str("  <Owner>\n");
    xml.push_str("    <ID>blu</ID>\n");
    xml.push_str("    <DisplayName>blu</DisplayName>\n");
    xml.push_str("  </Owner>\n");
    xml.push_str("  <Buckets>\n");
    xml.push_str("    <Bucket>\n");
    xml.push_str(&format!("      <Name>{}</Name>\n", xml_escape(bucket_name)));
    xml.push_str(&format!(
        "      <CreationDate>{}</CreationDate>\n",
        creation_date
    ));
    xml.push_str("    </Bucket>\n");
    xml.push_str("  </Buckets>\n");
    xml.push_str("</ListAllMyBucketsResult>\n");
    xml
}

/// Build the `InitiateMultipartUploadResult` XML response for
/// `CreateMultipartUpload`.
pub(crate) fn initiate_multipart_upload(bucket: &str, key: &str, upload_id: &str) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<InitiateMultipartUploadResult xmlns=\"{}\">\n",
        S3_NS
    ));
    xml.push_str(&format!("  <Bucket>{}</Bucket>\n", xml_escape(bucket)));
    xml.push_str(&format!("  <Key>{}</Key>\n", xml_escape(key)));
    xml.push_str(&format!(
        "  <UploadId>{}</UploadId>\n",
        xml_escape(upload_id)
    ));
    xml.push_str("</InitiateMultipartUploadResult>\n");
    xml
}

/// Build the `CompleteMultipartUploadResult` XML response for
/// `CompleteMultipartUpload`. `location` is the canonical object URI
/// (e.g., `http://127.0.0.1:7777/bucket/key`); `etag` is the final
/// object ETag (file hash wrapped in double quotes).
pub(crate) fn complete_multipart_upload(
    location: &str,
    bucket: &str,
    key: &str,
    etag: &str,
) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<CompleteMultipartUploadResult xmlns=\"{}\">\n",
        S3_NS
    ));
    xml.push_str(&format!(
        "  <Location>{}</Location>\n",
        xml_escape(location)
    ));
    xml.push_str(&format!("  <Bucket>{}</Bucket>\n", xml_escape(bucket)));
    xml.push_str(&format!("  <Key>{}</Key>\n", xml_escape(key)));
    xml.push_str(&format!("  <ETag>{}</ETag>\n", xml_escape(etag)));
    xml.push_str("</CompleteMultipartUploadResult>\n");
    xml
}

/// Build an S3 XML error response.
pub(crate) fn error_response(
    status: StatusCode,
    code: &str,
    message: &str,
) -> (
    StatusCode,
    [(axum::http::header::HeaderName, &'static str); 1],
    String,
) {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<Error>\n");
    xml.push_str(&format!("  <Code>{}</Code>\n", xml_escape(code)));
    xml.push_str(&format!("  <Message>{}</Message>\n", xml_escape(message)));
    xml.push_str("</Error>\n");

    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/xml")],
        xml,
    )
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn xml_escape_special_chars() {
        assert_eq!(xml_escape("a&b"), "a&amp;b");
        assert_eq!(xml_escape("a<b"), "a&lt;b");
        assert_eq!(xml_escape("a>b"), "a&gt;b");
        assert_eq!(xml_escape("a\"b"), "a&quot;b");
        assert_eq!(xml_escape("a'b"), "a&apos;b");
        assert_eq!(xml_escape("normal"), "normal");
    }

    #[test]
    fn continuation_token_round_trip() {
        let cursor = "docs/api/v2.md";
        let token = encode_continuation_token(cursor);
        assert_eq!(decode_continuation_token(&token), Some(cursor.to_string()));
    }

    #[test]
    fn continuation_token_invalid_input() {
        assert_eq!(decode_continuation_token("!!!invalid-base64!!!"), None);
    }

    #[test]
    fn group_no_delimiter_all_contents() {
        let entries = vec![
            ("a/b.txt".to_string(), Hash::from("1e20aaaa")),
            ("c.txt".to_string(), Hash::from("1e20bbbb")),
        ];
        let (contents, prefixes, cursor) = group_by_delimiter(&entries, "", None);
        assert_eq!(contents.len(), 2);
        assert!(prefixes.is_empty());
        assert_eq!(cursor, Some("c.txt".to_string()));
    }

    #[test]
    fn group_with_delimiter() {
        let entries = vec![
            ("docs/a.txt".to_string(), Hash::from("1e20aaaa")),
            ("docs/b.txt".to_string(), Hash::from("1e20bbbb")),
            ("photos/c.jpg".to_string(), Hash::from("1e20cccc")),
            ("readme.md".to_string(), Hash::from("1e20dddd")),
        ];
        let (contents, prefixes, cursor) = group_by_delimiter(&entries, "", Some("/"));

        // "readme.md" has no delimiter, goes to Contents.
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].0, "readme.md");

        // "docs/" and "photos/" are common prefixes.
        assert_eq!(prefixes.len(), 2);
        assert!(prefixes.contains("docs/"));
        assert!(prefixes.contains("photos/"));

        // The last entry is "readme.md" (Contents), so cursor is that key.
        assert_eq!(cursor, Some("readme.md".to_string()));
    }

    #[test]
    fn group_with_delimiter_and_prefix() {
        let entries = vec![
            ("docs/api/intro.md".to_string(), Hash::from("1e20aaaa")),
            ("docs/api/v2.md".to_string(), Hash::from("1e20bbbb")),
            ("docs/changelog.txt".to_string(), Hash::from("1e20cccc")),
        ];
        let (contents, prefixes, _cursor) = group_by_delimiter(&entries, "docs/", Some("/"));

        // "docs/changelog.txt" has no delimiter after "docs/", so it
        // goes to Contents.
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].0, "docs/changelog.txt");

        // "docs/api/" is a common prefix.
        assert_eq!(prefixes.len(), 1);
        assert!(prefixes.contains("docs/api/"));
    }

    #[test]
    fn group_cursor_skips_common_prefix() {
        let entries = vec![
            ("docs/api/a.md".to_string(), Hash::from("1e20aaaa")),
            ("docs/api/b.md".to_string(), Hash::from("1e20bbbb")),
        ];
        let (_contents, _prefixes, cursor) = group_by_delimiter(&entries, "docs/", Some("/"));

        // The last entry is a common prefix "docs/api/", so the cursor
        // should be the next prefix after "docs/api/" to skip all keys
        // under it on the next page.
        assert_eq!(cursor, Some("docs/api0".to_string()));
    }

    #[test]
    fn next_prefix_str_simple() {
        assert_eq!(next_prefix_str("docs/api/"), "docs/api0");
        assert_eq!(next_prefix_str("a"), "b");
    }

    #[test]
    fn list_all_my_buckets_xml() {
        let xml = list_all_my_buckets("myvault", "2026-06-19T00:00:00.000Z");
        assert!(xml.contains("ListAllMyBucketsResult"));
        assert!(xml.contains("<Name>myvault</Name>"));
        assert!(xml.contains("<CreationDate>2026-06-19T00:00:00.000Z</CreationDate>"));
    }

    #[test]
    fn list_bucket_result_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store =
            crate::serve::redb_store::RedbStore::open(&tmp.path().join("test.redb")).unwrap();
        let dt = from_epoch(1718774400);

        let xml = list_bucket_result(
            "myvault",
            "",
            None,
            1000,
            &None,
            &None,
            false,
            false,
            None,
            &[],
            &BTreeSet::new(),
            &dt,
            &store,
        );

        assert!(xml.contains("ListBucketResult"));
        assert!(xml.contains("<Name>myvault</Name>"));
        assert!(xml.contains("<Prefix></Prefix>"));
        assert!(xml.contains("<KeyCount>0</KeyCount>"));
        assert!(xml.contains("<MaxKeys>1000</MaxKeys>"));
        assert!(xml.contains("<IsTruncated>false</IsTruncated>"));
        assert!(!xml.contains("<Contents>"));
        assert!(!xml.contains("<CommonPrefixes>"));
        assert!(!xml.contains("<NextContinuationToken>"));
    }

    #[test]
    fn list_bucket_result_with_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let store =
            crate::serve::redb_store::RedbStore::open(&tmp.path().join("test.redb")).unwrap();

        // Populate with a file so get_fileref returns a size.
        use crate::blob::BlobIndex;
        use crate::block::{ChunkMeta, FileRef, PlainIndex};
        use crate::tag::TagIndex;
        use std::collections::HashSet;
        use std::path::PathBuf;

        let mut plain = PlainIndex::new_empty();
        let chunk = ChunkMeta {
            hash: Hash::from("1e20aaaa"),
            size: 4096,
        };
        let fileref = FileRef {
            chunkmetas: vec![chunk],
            paths: HashSet::from([PathBuf::from("docs/readme.txt")]),
        };
        let file_hash = Hash::from("1e20bbbb");
        plain.files.insert(file_hash.clone(), fileref);
        store
            .populate_from_indexes(&plain, &BlobIndex::default(), &TagIndex::new())
            .unwrap();

        let dt = from_epoch(1718774400);
        let entries = vec![("docs/readme.txt".to_string(), file_hash)];

        let xml = list_bucket_result(
            "myvault",
            "",
            None,
            1000,
            &None,
            &None,
            false,
            false,
            None,
            &entries,
            &BTreeSet::new(),
            &dt,
            &store,
        );

        assert!(xml.contains("<Key>docs/readme.txt</Key>"));
        assert!(xml.contains("<Size>4096</Size>"));
        assert!(xml.contains("<StorageClass>STANDARD</StorageClass>"));
        // ETag should be the hash wrapped in double quotes.
        assert!(xml.contains("<ETag>\""));
        assert!(xml.contains("<LastModified>"));
        assert!(xml.contains("2024-06-19T"));
    }

    #[test]
    fn list_bucket_result_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        let store =
            crate::serve::redb_store::RedbStore::open(&tmp.path().join("test.redb")).unwrap();
        let dt = from_epoch(1718774400);

        let xml = list_bucket_result(
            "myvault",
            "",
            None,
            1000,
            &None,
            &None,
            false,
            true,
            Some("dGVzdA=="),
            &[],
            &BTreeSet::new(),
            &dt,
            &store,
        );

        assert!(xml.contains("<IsTruncated>true</IsTruncated>"));
        assert!(xml.contains("<NextContinuationToken>dGVzdA==</NextContinuationToken>"));
    }

    #[test]
    fn list_bucket_result_with_delimiter() {
        let tmp = tempfile::tempdir().unwrap();
        let store =
            crate::serve::redb_store::RedbStore::open(&tmp.path().join("test.redb")).unwrap();
        let dt = from_epoch(1718774400);

        let mut prefixes = BTreeSet::new();
        prefixes.insert("docs/".to_string());
        prefixes.insert("photos/".to_string());

        let xml = list_bucket_result(
            "myvault",
            "",
            Some("/"),
            1000,
            &None,
            &None,
            false,
            false,
            None,
            &[],
            &prefixes,
            &dt,
            &store,
        );

        assert!(xml.contains("<Delimiter>/</Delimiter>"));
        assert!(xml.contains("<CommonPrefixes>"));
        assert!(xml.contains("<Prefix>docs/</Prefix>"));
        assert!(xml.contains("<Prefix>photos/</Prefix>"));
        assert!(xml.contains("<KeyCount>2</KeyCount>"));
    }
}
