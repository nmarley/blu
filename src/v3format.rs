//! v3 segmented AEAD blob format.
//!
//! v3 replaces v2's single sealed AEAD box with fixed-size,
//! independently authenticated segments. Each segment is encrypted
//! with the blob's DEK using a counter-derived nonce, so a reader can
//! fetch and decrypt a prefix of the blob (the segments covering a
//! chunk's compressed bytes) without downloading the whole thing.
//!
//! Index files (`BLUI`) remain v2; they are always read whole and gain
//! nothing from segmentation.
//!
//! ## File layout
//!
//! ```text
//! Offset   Size     Field
//! 0        4        Magic: "BLUB" (same as v2)
//! 4        2        Format version: 3 (LE u16)
//! 6        2        KEK version (LE u16)
//! 8        4        Wrapped DEK length N (LE u32)
//! 12       N        Wrapped DEK (nonce || ciphertext || tag)
//! 12+N     4        Segment size S in bytes (LE u32)
//! 16+N     4        Segment count K (LE u32)
//! 20+N     8        Compressed plaintext length P (LE u64)
//! 28+N     ...      K segments, each exactly S + 16 bytes
//! ```
//!
//! `P` is the length of the compressed stream before padding. The
//! reader uses it to trim padding from the final segment after
//! decompression.
//!
//! See `docs/design/BLU_SERVE_DESIGN.md` section 5 for the full rationale.

use std::io::{self, Write};

use crate::error::{BluError, Result};

/// v3 format version.
pub const FORMAT_VERSION_V3: u16 = 3;

/// The v3 header fields that follow the shared v2-style prefix (magic,
/// version, kek_version, wrapped_dek). These are specific to the
/// segmented format.
const V3_HEADER_TAIL_SIZE: usize = 4 + 4 + 8; // segment_size + segment_count + plaintext_len

/// A parsed v3 blob header.
#[derive(Debug, Clone)]
pub struct V3Header {
    /// Which KEK version was used to wrap the DEK.
    pub kek_version: u16,
    /// The wrapped DEK bytes (nonce || ciphertext || tag).
    pub wrapped_dek: Vec<u8>,
    /// Segment size S in bytes. Each segment's plaintext is exactly S
    /// bytes (the final segment is zero-padded).
    pub segment_size: u32,
    /// Number of segments K in the blob.
    pub segment_count: u32,
    /// Length of the compressed stream before padding. The reader uses
    /// this to trim padding from the decompressed output.
    pub plaintext_len: u64,
}

impl V3Header {
    /// The on-disk size of the full v3 header (magic + version +
    /// kek_version + wrapped_dek_len + wrapped_dek + v3 tail fields).
    pub fn header_size(&self) -> usize {
        4 + 2 + 2 + 4 + self.wrapped_dek.len() + V3_HEADER_TAIL_SIZE
    }

    /// The on-disk size of a single segment (plaintext segment + tag).
    /// The nonce is not stored inline (it is counter-derived).
    pub fn on_disk_segment_size(&self) -> usize {
        self.segment_size as usize + 16
    }

    /// The byte offset where segment 0 begins (i.e., the end of the
    /// full header).
    pub fn payload_offset(&self) -> usize {
        self.header_size()
    }

    /// The total on-disk size of all segments combined.
    pub fn segments_size(&self) -> usize {
        self.segment_count as usize * self.on_disk_segment_size()
    }

    /// The total on-disk size of the blob (header + all segments).
    pub fn total_size(&self) -> usize {
        self.header_size() + self.segments_size()
    }
}

/// Read the 2-byte format version from raw file data without fully
/// parsing the header. Returns `None` if the data is too short or does
/// not start with a `BLUB`/`BLUI` magic.
pub fn peek_version(data: &[u8]) -> Option<u16> {
    if data.len() < 6 {
        return None;
    }
    let magic = &data[0..4];
    if magic != crate::v2format::MAGIC_BLOB && magic != crate::v2format::MAGIC_INDEX {
        return None;
    }
    Some(u16::from_le_bytes([data[4], data[5]]))
}

/// Check whether raw file data is a v3 blob (magic + version 3).
pub fn is_v3(data: &[u8]) -> bool {
    peek_version(data) == Some(FORMAT_VERSION_V3)
}

/// Parse a v3 header from raw file data.
///
/// Returns the header and the offset where the segment payload begins.
pub fn read_header(data: &[u8]) -> Result<(V3Header, usize)> {
    // Reuse v2's header parsing for the shared prefix (magic, version,
    // kek_version, wrapped_dek). v2's read_header checks the version
    // is FORMAT_VERSION (2), so we can't call it directly for v3.
    // Instead, parse the shared fields manually and then read the v3
    // tail.

    if data.len() < 4 + 2 + 2 + 4 {
        return Err(BluError::DecryptionFailed(
            "v3 file too short for header".into(),
        ));
    }

    let magic = &data[0..4];
    if magic != crate::v2format::MAGIC_BLOB {
        return Err(BluError::DecryptionFailed(format!(
            "v3 blob has wrong magic: {:02x}{:02x}{:02x}{:02x} (expected BLUB)",
            magic[0], magic[1], magic[2], magic[3]
        )));
    }

    let format_version = u16::from_le_bytes([data[4], data[5]]);
    if format_version != FORMAT_VERSION_V3 {
        return Err(BluError::DecryptionFailed(format!(
            "unsupported format version: {} (expected {})",
            format_version, FORMAT_VERSION_V3
        )));
    }

    let kek_version = u16::from_le_bytes([data[6], data[7]]);
    let wrapped_dek_len = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;

    let shared_header_size = 4 + 2 + 2 + 4 + wrapped_dek_len;
    if data.len() < shared_header_size {
        return Err(BluError::DecryptionFailed(format!(
            "v3 file truncated: need {} bytes for shared header, got {}",
            shared_header_size,
            data.len()
        )));
    }

    let wrapped_dek = data[12..shared_header_size].to_vec();

    // v3 tail: segment_size (4) + segment_count (4) + plaintext_len (8)
    let tail_end = shared_header_size + V3_HEADER_TAIL_SIZE;
    if data.len() < tail_end {
        return Err(BluError::DecryptionFailed(format!(
            "v3 file truncated: need {} bytes for full header, got {}",
            tail_end,
            data.len()
        )));
    }

    let mut tail = &data[shared_header_size..tail_end];
    let segment_size = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    tail = &tail[4..];
    let segment_count = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    tail = &tail[4..];
    let plaintext_len = u64::from_le_bytes([
        tail[0], tail[1], tail[2], tail[3], tail[4], tail[5], tail[6], tail[7],
    ]);

    if segment_size == 0 {
        return Err(BluError::DecryptionFailed(
            "v3 header has segment_size of 0".into(),
        ));
    }

    let header = V3Header {
        kek_version,
        wrapped_dek,
        segment_size,
        segment_count,
        plaintext_len,
    };

    Ok((header, tail_end))
}

/// Write a v3 header to a writer.
fn write_header<W: Write>(
    writer: &mut W,
    kek_version: u16,
    wrapped_dek: &[u8],
    segment_size: u32,
    segment_count: u32,
    plaintext_len: u64,
) -> io::Result<()> {
    writer.write_all(&crate::v2format::MAGIC_BLOB)?;
    writer.write_all(&FORMAT_VERSION_V3.to_le_bytes())?;
    writer.write_all(&kek_version.to_le_bytes())?;
    writer.write_all(&(wrapped_dek.len() as u32).to_le_bytes())?;
    writer.write_all(wrapped_dek)?;
    writer.write_all(&segment_size.to_le_bytes())?;
    writer.write_all(&segment_count.to_le_bytes())?;
    writer.write_all(&plaintext_len.to_le_bytes())?;
    Ok(())
}

/// Assemble a v3 blob from pre-computed components.
///
/// Used by the writer path where the DEK wrapping happens via
/// `DekProvider` and segment encryption happens in-process. Writes the
/// v3 header followed by the already-encrypted segment bytes (each
/// segment is `ciphertext || tag`, no inline nonce).
pub fn write_v3<W: Write>(
    writer: &mut W,
    kek_version: u16,
    wrapped_dek: &[u8],
    segment_size: u32,
    segment_count: u32,
    plaintext_len: u64,
    encrypted_segments: &[u8],
) -> io::Result<()> {
    write_header(
        writer,
        kek_version,
        wrapped_dek,
        segment_size,
        segment_count,
        plaintext_len,
    )?;
    writer.write_all(encrypted_segments)?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    fn fake_wrapped_dek() -> Vec<u8> {
        vec![0xAA; 60] // simulated wrapped DEK (nonce + ciphertext + tag)
    }

    #[test]
    fn peek_version_v2() {
        let mut data = Vec::new();
        data.extend_from_slice(&crate::v2format::MAGIC_BLOB);
        data.extend_from_slice(&2u16.to_le_bytes());
        assert_eq!(peek_version(&data), Some(2));
    }

    #[test]
    fn peek_version_v3() {
        let mut data = Vec::new();
        data.extend_from_slice(&crate::v2format::MAGIC_BLOB);
        data.extend_from_slice(&FORMAT_VERSION_V3.to_le_bytes());
        assert_eq!(peek_version(&data), Some(FORMAT_VERSION_V3));
    }

    #[test]
    fn peek_version_too_short() {
        assert_eq!(peek_version(&[0x42, 0x4C]), None);
    }

    #[test]
    fn peek_version_bad_magic() {
        let mut data = Vec::new();
        data.extend_from_slice(b"XXXX");
        data.extend_from_slice(&FORMAT_VERSION_V3.to_le_bytes());
        assert_eq!(peek_version(&data), None);
    }

    #[test]
    fn is_v3_detects_v3() {
        let mut data = Vec::new();
        data.extend_from_slice(&crate::v2format::MAGIC_BLOB);
        data.extend_from_slice(&FORMAT_VERSION_V3.to_le_bytes());
        data.extend_from_slice(&[0u8; 20]); // padding
        assert!(is_v3(&data));
    }

    #[test]
    fn is_v3_rejects_v2() {
        let mut data = Vec::new();
        data.extend_from_slice(&crate::v2format::MAGIC_BLOB);
        data.extend_from_slice(&2u16.to_le_bytes());
        assert!(!is_v3(&data));
    }

    #[test]
    fn header_round_trip() {
        let wrapped_dek = fake_wrapped_dek();
        let segment_size: u32 = 524_288; // 512 KiB
        let segment_count: u32 = 4;
        let plaintext_len: u64 = 1_000_000;

        let mut buf = Vec::new();
        write_header(
            &mut buf,
            7,
            &wrapped_dek,
            segment_size,
            segment_count,
            plaintext_len,
        )
        .unwrap();

        // Append fake segment payload so the data looks complete.
        let seg_size = segment_size as usize + 16;
        buf.extend_from_slice(&vec![0xBB; segment_count as usize * seg_size]);

        let (header, offset) = read_header(&buf).unwrap();
        assert_eq!(header.kek_version, 7);
        assert_eq!(header.wrapped_dek, wrapped_dek);
        assert_eq!(header.segment_size, segment_size);
        assert_eq!(header.segment_count, segment_count);
        assert_eq!(header.plaintext_len, plaintext_len);

        // Payload offset should be right after the header.
        let expected_offset = 4 + 2 + 2 + 4 + wrapped_dek.len() + V3_HEADER_TAIL_SIZE;
        assert_eq!(offset, expected_offset);
        assert_eq!(header.payload_offset(), expected_offset);
    }

    #[test]
    fn read_header_truncated_shared_prefix() {
        // Only 6 bytes, not enough for the shared header.
        let mut data = Vec::new();
        data.extend_from_slice(&crate::v2format::MAGIC_BLOB);
        data.extend_from_slice(&FORMAT_VERSION_V3.to_le_bytes());
        let result = read_header(&data);
        assert!(result.is_err());
    }

    #[test]
    fn read_header_truncated_tail() {
        let wrapped_dek = fake_wrapped_dek();
        // Write the shared header but only part of the v3 tail.
        let mut buf = Vec::new();
        buf.extend_from_slice(&crate::v2format::MAGIC_BLOB);
        buf.extend_from_slice(&FORMAT_VERSION_V3.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // kek_version
        buf.extend_from_slice(&(wrapped_dek.len() as u32).to_le_bytes());
        buf.extend_from_slice(&wrapped_dek);
        // Only 4 bytes of the 16-byte tail.
        buf.extend_from_slice(&512u32.to_le_bytes());

        let result = read_header(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn read_header_wrong_version() {
        let mut data = Vec::new();
        data.extend_from_slice(&crate::v2format::MAGIC_BLOB);
        data.extend_from_slice(&99u16.to_le_bytes()); // bad version
        data.extend_from_slice(&[0u8; 30]);
        let result = read_header(&data);
        assert!(result.is_err());
    }

    #[test]
    fn read_header_wrong_magic() {
        let mut data = Vec::new();
        data.extend_from_slice(b"BLUI"); // index magic, not blob
        data.extend_from_slice(&FORMAT_VERSION_V3.to_le_bytes());
        data.extend_from_slice(&[0u8; 30]);
        let result = read_header(&data);
        assert!(result.is_err());
    }

    #[test]
    fn read_header_zero_segment_size_errors() {
        let wrapped_dek = fake_wrapped_dek();
        let mut buf = Vec::new();
        write_header(&mut buf, 0, &wrapped_dek, 0, 1, 100).unwrap();
        // Append fake payload.
        buf.extend_from_slice(&[0u8; 16]);

        let result = read_header(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn write_v3_assembles_header_and_segments() {
        let wrapped_dek = fake_wrapped_dek();
        let segment_size: u32 = 1024;
        let segment_count: u32 = 2;
        let plaintext_len: u64 = 2048;

        // Fake encrypted segments (each segment_size + 16 bytes).
        let seg_bytes = vec![0xCC; segment_count as usize * (segment_size as usize + 16)];

        let mut buf = Vec::new();
        write_v3(
            &mut buf,
            3,
            &wrapped_dek,
            segment_size,
            segment_count,
            plaintext_len,
            &seg_bytes,
        )
        .unwrap();

        assert!(is_v3(&buf));

        let (header, offset) = read_header(&buf).unwrap();
        assert_eq!(header.kek_version, 3);
        assert_eq!(header.segment_size, segment_size);
        assert_eq!(header.segment_count, segment_count);
        assert_eq!(header.plaintext_len, plaintext_len);

        // The segment payload starts at offset and should be exactly seg_bytes.
        assert_eq!(&buf[offset..], &seg_bytes[..]);
        assert_eq!(header.total_size(), buf.len());
    }

    #[test]
    fn header_size_calculations() {
        let header = V3Header {
            kek_version: 0,
            wrapped_dek: vec![0xAA; 60],
            segment_size: 524_288,
            segment_count: 128,
            plaintext_len: 67_108_864,
        };

        // header = 4 + 2 + 2 + 4 + 60 + 16 = 88
        assert_eq!(header.header_size(), 88);
        assert_eq!(header.payload_offset(), 88);
        // on_disk_segment = 524288 + 16 = 524304
        assert_eq!(header.on_disk_segment_size(), 524_304);
        // segments = 128 * 524304 = 67_110_912
        assert_eq!(header.segments_size(), 67_110_912);
        // total = 88 + 67_110_912 = 67_111_000
        assert_eq!(header.total_size(), 67_111_000);
    }
}
