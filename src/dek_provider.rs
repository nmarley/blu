//! Envelope key provider for wrap/unwrap of Data Encryption Keys.
//!
//! `DekProvider` is the central abstraction for key management in the
//! envelope encryption scheme. It handles only KEK/DEK wrapping, while
//! bulk data encryption is performed locally by free functions in this
//! module.
//!
//! Two variants exist:
//!
//! - `Local`: holds an unwrapped KEK in-process. Used during vault
//!   initialization (before the agent daemon is involved).
//! - `Agent`: delegates key wrapping to the agent daemon over a Unix
//!   socket. Key material never leaves the daemon process.

use crate::agent::AgentClient;
use crate::error::{BluError, Result};
use crate::keys::dek::{Dek, SegmentAad};
use crate::keys::kek::Kek;
use crate::v2format::{self, FileType};
use crate::v3format;

/// Provides DEK wrapping and unwrapping using the vault's KEK.
///
/// This is the key management seam in the envelope encryption scheme.
/// All bulk data encryption happens locally with a DEK; `DekProvider`
/// controls only who holds the KEK and how DEKs are wrapped/unwrapped.
pub enum DekProvider {
    /// KEK held in the current process.
    ///
    /// Used during `blu init` (vault creation) before the agent daemon
    /// is involved. The KEK and its version are held directly.
    Local {
        /// The unwrapped KEK for this session.
        kek: Kek,
        /// Which KEK version this is (written into v2 headers).
        kek_version: u16,
    },
    /// KEK held by the agent daemon.
    ///
    /// The agent manages the KEK lifecycle (loading from disk, caching,
    /// zeroizing on lock/timeout). The client sends wrap/unwrap RPCs
    /// over a Unix socket; plaintext key material never crosses the
    /// process boundary except for ephemeral DEKs.
    Agent {
        /// Client connection to the agent daemon.
        client: AgentClient,
        /// Path to the vault's `.blu/` directory, sent to the agent so
        /// it can lazily load the correct KEK on first use.
        kek_dir: Option<String>,
    },
}

impl Clone for DekProvider {
    fn clone(&self) -> Self {
        match self {
            DekProvider::Local { kek, kek_version } => DekProvider::Local {
                kek: kek.clone(),
                kek_version: *kek_version,
            },
            DekProvider::Agent { kek_dir, .. } => {
                let client = AgentClient::new()
                    .expect("failed to create agent client for DekProvider clone");
                DekProvider::Agent {
                    client,
                    kek_dir: kek_dir.clone(),
                }
            }
        }
    }
}

impl std::fmt::Debug for DekProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DekProvider::Local { kek_version, .. } => f
                .debug_struct("DekProvider::Local")
                .field("kek_version", kek_version)
                .finish(),
            DekProvider::Agent { kek_dir, .. } => f
                .debug_struct("DekProvider::Agent")
                .field("kek_dir", kek_dir)
                .finish(),
        }
    }
}

impl DekProvider {
    /// Generate a fresh DEK and wrap it with the KEK.
    ///
    /// Returns the plaintext DEK (for encrypting data locally), the
    /// wrapped DEK bytes (for storing in the file header), and the
    /// KEK version used.
    pub fn wrap_dek(&self) -> Result<(Dek, Vec<u8>, u16)> {
        match self {
            DekProvider::Local { kek, kek_version } => {
                let dek = Dek::generate();
                let wrapped = dek.wrap(kek)?;
                Ok((dek, wrapped, *kek_version))
            }
            DekProvider::Agent { client, kek_dir } => {
                let (dek_bytes, wrapped_dek, kek_version) = client.wrap_dek(kek_dir.as_deref())?;
                let dek = Dek::from_bytes(&dek_bytes)?;
                Ok((dek, wrapped_dek, kek_version))
            }
        }
    }

    /// Unwrap a DEK from its wrapped form using the KEK.
    ///
    /// The `version` parameter is the KEK version stored in the file
    /// header. For the `Local` variant, it must match the version held
    /// by this provider; otherwise an error is returned. For the
    /// `Agent` variant, version validation is handled by the daemon.
    pub fn unwrap_dek(&self, wrapped: &[u8], version: u16) -> Result<Dek> {
        match self {
            DekProvider::Local { kek, kek_version } => {
                if version != *kek_version {
                    return Err(BluError::DecryptionFailed(format!(
                        "KEK version mismatch: file requires v{}, provider has v{}",
                        version, kek_version
                    )));
                }
                Dek::unwrap(kek, wrapped)
            }
            DekProvider::Agent { client, kek_dir } => {
                let dek_bytes = client.unwrap_dek(wrapped, version, kek_dir.as_deref())?;
                Dek::from_bytes(&dek_bytes)
            }
        }
    }
}

/// Encrypt data in v2 envelope format.
///
/// Wraps a fresh DEK with the provider's KEK, encrypts the payload
/// with ChaCha20-Poly1305, and assembles the complete file
/// (header + encrypted payload).
pub fn encrypt_envelope(data: &[u8], file_type: FileType, keys: &DekProvider) -> Result<Vec<u8>> {
    let (dek, wrapped_dek, kek_version) = keys.wrap_dek()?;
    let encrypted_payload = dek.encrypt_data(data)?;

    let mut output = Vec::new();
    v2format::write_v2(
        &mut output,
        file_type,
        kek_version,
        &wrapped_dek,
        &encrypted_payload,
    )
    .map_err(|e| BluError::EncryptionFailed(e.to_string()))?;

    Ok(output)
}

/// Decrypt v2 envelope-encrypted data.
///
/// Parses the file header, unwraps the DEK via the provider, and
/// decrypts the payload with ChaCha20-Poly1305.
pub fn decrypt_envelope(data: &[u8], keys: &DekProvider) -> Result<Vec<u8>> {
    if !v2format::is_v2(data) {
        return Err(BluError::DecryptionFailed(
            "not a v2 envelope-encrypted file".into(),
        ));
    }

    let (header, payload_offset) = v2format::read_header(data)?;
    let dek = keys.unwrap_dek(&header.wrapped_dek, header.kek_version)?;
    let payload = &data[payload_offset..];
    dek.decrypt_data(payload)
}

/// The index of the last segment covering a chunk whose compressed
/// bytes end at `compressed_end`.
///
/// Segments `0..=last_segment_for(..)` must be fetched and decrypted to
/// recover the chunk. Because `compressed_end` is one-past-the-last
/// compressed byte, the last covered byte is at index
/// `compressed_end - 1`, which lives in segment
/// `(compressed_end - 1) / segment_size`. A `compressed_end` of 0 (an
/// empty leading region) maps to segment 0.
pub fn last_segment_for(compressed_end: u64, segment_size: u32) -> u32 {
    let segment_size = segment_size as u64;
    let last_byte = compressed_end.saturating_sub(1);
    (last_byte / segment_size) as u32
}

/// Encrypt an already-compressed stream into a v3 segmented blob.
///
/// The compressed input is zero-padded up to a `segment_size` multiple
/// and split into `ceil(len / segment_size)` fixed-size segments, each
/// encrypted independently with the blob's DEK under a
/// counter-derived nonce (see [`Dek::encrypt_segment`]). The result is
/// a complete v3 file: header (recording `plaintext_len =
/// compressed.len()`, the pre-pad length) followed by the concatenated
/// `ciphertext || tag` records.
///
/// This is the crypto seam only: it takes bare compressed bytes and
/// returns bare file bytes. Chunk-boundary bookkeeping
/// (`compressed_end` per chunk) is the caller's concern, produced by
/// [`crate::compression::compress_with_progress`].
pub fn encrypt_envelope_segmented(
    compressed: &[u8],
    segment_size: usize,
    keys: &DekProvider,
) -> Result<Vec<u8>> {
    if segment_size == 0 {
        return Err(BluError::EncryptionFailed(
            "segment_size must be non-zero".into(),
        ));
    }

    let (dek, wrapped_dek, kek_version) = keys.wrap_dek()?;

    let plaintext_len = compressed.len();
    let segment_count = plaintext_len.div_ceil(segment_size).max(1);
    let padded_len = segment_count * segment_size;

    let aad = SegmentAad {
        segment_size: segment_size as u32,
        segment_count: segment_count as u32,
        plaintext_len: plaintext_len as u64,
    };

    // Zero-pad the final segment up to a full segment_size.
    let mut padded = Vec::with_capacity(padded_len);
    padded.extend_from_slice(compressed);
    padded.resize(padded_len, 0);

    let mut encrypted_segments = Vec::with_capacity(segment_count * (segment_size + 16));
    for i in 0..segment_count {
        let start = i * segment_size;
        let end = start + segment_size;
        let record = dek.encrypt_segment(i as u64, &aad, &padded[start..end])?;
        encrypted_segments.extend_from_slice(&record);
    }

    let mut output = Vec::new();
    v3format::write_v3(
        &mut output,
        kek_version,
        &wrapped_dek,
        segment_size as u32,
        segment_count as u32,
        plaintext_len as u64,
        &encrypted_segments,
    )
    .map_err(|e| BluError::EncryptionFailed(e.to_string()))?;

    Ok(output)
}

/// Decrypt and decompress a prefix of a v3 segmented blob.
///
/// Given the full blob bytes `data` (Stage 6f will pass only a fetched
/// prefix), decrypt segments `0..=up_to_seg`, concatenate the
/// resulting compressed bytes, and decompress. When `up_to_seg` is the
/// final segment the whole compressed stream is present, so a normal
/// decompress runs and the trailing zero padding after the gzip
/// trailer is ignored. Otherwise a prefix decompress runs, returning
/// the leading decompressed bytes that the fetched segments cover.
///
/// The returned bytes are a prefix of the fully-decompressed blob: any
/// chunk whose decompressed end falls within this prefix can be sliced
/// out of it.
pub fn decrypt_envelope_segmented_prefix(
    data: &[u8],
    up_to_seg: u32,
    keys: &DekProvider,
) -> Result<Vec<u8>> {
    let (header, payload_offset) = v3format::read_header(data)?;

    if up_to_seg >= header.segment_count {
        return Err(BluError::DecryptionFailed(format!(
            "requested segment {} but blob has only {} segments",
            up_to_seg, header.segment_count
        )));
    }

    let dek = keys.unwrap_dek(&header.wrapped_dek, header.kek_version)?;

    let aad = SegmentAad {
        segment_size: header.segment_size,
        segment_count: header.segment_count,
        plaintext_len: header.plaintext_len,
    };

    let on_disk_segment = header.on_disk_segment_size();
    let mut compressed =
        Vec::with_capacity((up_to_seg as usize + 1) * header.segment_size as usize);

    for i in 0..=up_to_seg {
        let start = payload_offset + i as usize * on_disk_segment;
        let end = start + on_disk_segment;
        if data.len() < end {
            return Err(BluError::DecryptionFailed(format!(
                "v3 blob truncated: need {} bytes for segment {}, got {}",
                end,
                i,
                data.len()
            )));
        }
        let record = &data[start..end];
        let plain = dek.decrypt_segment(i as u64, &aad, record)?;
        compressed.extend_from_slice(&plain);
    }

    let is_full = up_to_seg == header.segment_count - 1;
    if is_full {
        // The full compressed stream (plus zero padding) is present.
        // Trim to plaintext_len so the gzip trailer terminates cleanly
        // and the post-trailer padding is excluded. A plaintext_len
        // larger than the decrypted bytes indicates a tampered or
        // corrupted header; return a clean error rather than panicking.
        let plaintext_len = header.plaintext_len as usize;
        if plaintext_len > compressed.len() {
            return Err(BluError::DecryptionFailed(format!(
                "v3 header plaintext_len {} exceeds decrypted segment bytes {}",
                plaintext_len,
                compressed.len()
            )));
        }
        let trimmed = &compressed[..plaintext_len];
        crate::compression::decompress(trimmed)
            .map_err(|e| BluError::DecryptionFailed(e.to_string()))
    } else {
        // A compressed prefix: decompress as far as the bytes allow.
        crate::compression::decompress_prefix(&compressed)
            .map_err(|e| BluError::DecryptionFailed(e.to_string()))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::keys::kek::Kek;

    fn local_provider(kek: &Kek, version: u16) -> DekProvider {
        DekProvider::Local {
            kek: kek.clone(),
            kek_version: version,
        }
    }

    #[test]
    fn encrypt_decrypt_blob() {
        let kek = Kek::generate();
        let keys = local_provider(&kek, 0);
        let data = b"blob data for v2";

        let encrypted = encrypt_envelope(data, FileType::Blob, &keys).unwrap();
        assert!(v2format::is_v2(&encrypted));

        let decrypted = decrypt_envelope(&encrypted, &keys).unwrap();
        assert_eq!(&decrypted, data);
    }

    #[test]
    fn encrypt_decrypt_index() {
        let kek = Kek::generate();
        let keys = local_provider(&kek, 5);
        let data = b"index data for v2";

        let encrypted = encrypt_envelope(data, FileType::Index, &keys).unwrap();
        assert!(v2format::is_v2(&encrypted));

        let decrypted = decrypt_envelope(&encrypted, &keys).unwrap();
        assert_eq!(&decrypted, data);
    }

    #[test]
    fn decrypt_non_v2_data_errors() {
        let kek = Kek::generate();
        let keys = local_provider(&kek, 0);

        let result = decrypt_envelope(b"not a v2 file at all", &keys);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_with_wrong_kek_errors() {
        let kek1 = Kek::generate();
        let kek2 = Kek::generate();
        let keys_write = local_provider(&kek1, 0);
        let keys_read = local_provider(&kek2, 0);

        let encrypted = encrypt_envelope(b"secret", FileType::Blob, &keys_write).unwrap();
        let result = decrypt_envelope(&encrypted, &keys_read);
        assert!(result.is_err());
    }

    #[test]
    fn version_mismatch_errors() {
        let kek = Kek::generate();
        let keys_v0 = local_provider(&kek, 0);
        let keys_v1 = local_provider(&kek, 1);

        let encrypted = encrypt_envelope(b"secret", FileType::Blob, &keys_v0).unwrap();
        let result = decrypt_envelope(&encrypted, &keys_v1);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("version mismatch"),
            "expected version mismatch error, got: {}",
            err_msg
        );
    }

    #[test]
    fn clone_preserves_local_state() {
        let kek = Kek::generate();
        let keys = local_provider(&kek, 3);
        let keys2 = keys.clone();

        let encrypted = encrypt_envelope(b"cloned", FileType::Blob, &keys).unwrap();
        let decrypted = decrypt_envelope(&encrypted, &keys2).unwrap();
        assert_eq!(&decrypted, b"cloned");
    }

    #[test]
    fn debug_does_not_leak_key_material() {
        let kek = Kek::generate();
        let keys = local_provider(&kek, 7);
        let debug_str = format!("{:?}", keys);
        assert!(debug_str.contains("kek_version: 7"));
        assert!(
            !debug_str.contains("kek:"),
            "debug output must not contain key material"
        );
    }

    #[test]
    fn last_segment_for_boundaries() {
        // segment_size 1000.
        assert_eq!(last_segment_for(0, 1000), 0);
        assert_eq!(last_segment_for(1, 1000), 0);
        assert_eq!(last_segment_for(1000, 1000), 0); // byte 999 -> seg 0
        assert_eq!(last_segment_for(1001, 1000), 1); // byte 1000 -> seg 1
        assert_eq!(last_segment_for(2000, 1000), 1); // byte 1999 -> seg 1
        assert_eq!(last_segment_for(2001, 1000), 2);
    }

    #[test]
    fn segmented_round_trip_full_read() {
        use crate::compression::compress_with_progress;

        let kek = Kek::generate();
        let keys = local_provider(&kek, 0);

        // Multi-region compressible payload.
        let mut data = Vec::new();
        let region_sizes = [4000usize, 5000, 6000];
        for (i, &sz) in region_sizes.iter().enumerate() {
            data.extend(std::iter::repeat(b'a' + i as u8).take(sz));
        }
        let mut endpoints = Vec::new();
        let mut acc = 0;
        for &sz in &region_sizes {
            acc += sz;
            endpoints.push(acc);
        }

        let (compressed, _ends) = compress_with_progress(&data, &endpoints).unwrap();
        let segment_size = 4096usize;
        let blob = encrypt_envelope_segmented(&compressed, segment_size, &keys).unwrap();

        assert!(crate::v3format::is_v3(&blob));

        let (header, _) = crate::v3format::read_header(&blob).unwrap();
        let last_seg = header.segment_count - 1;

        // Full read (up to the last segment) returns the whole input.
        let decoded = decrypt_envelope_segmented_prefix(&blob, last_seg, &keys).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn segmented_prefix_yields_leading_bytes() {
        use crate::compression::compress_with_progress;

        let kek = Kek::generate();
        let keys = local_provider(&kek, 0);

        // Low-compressibility payload so the compressed stream spans
        // several small segments (a highly-repetitive payload would
        // shrink into a single segment).
        let mut data = Vec::new();
        let mut state = 0x1234_5678_9abc_def0u64;
        for _ in 0..40_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.push((state & 0xff) as u8);
        }
        let endpoints = [data.len()];

        let (compressed, _ends) = compress_with_progress(&data, &endpoints).unwrap();
        let segment_size = 1024usize;
        let blob = encrypt_envelope_segmented(&compressed, segment_size, &keys).unwrap();

        let (header, _) = crate::v3format::read_header(&blob).unwrap();
        assert!(header.segment_count > 1, "test needs multiple segments");

        // Decrypt just the first segment: the decoded bytes must be a
        // leading prefix of the full data.
        let prefix = decrypt_envelope_segmented_prefix(&blob, 0, &keys).unwrap();
        assert!(!prefix.is_empty(), "front segment should decode some bytes");
        assert!(
            prefix.len() < data.len(),
            "front segment is a strict prefix"
        );
        assert_eq!(&data[..prefix.len()], &prefix[..]);
    }

    #[test]
    fn segmented_wrong_key_fails() {
        use crate::compression::compress_with_progress;

        let kek1 = Kek::generate();
        let kek2 = Kek::generate();
        let keys_write = local_provider(&kek1, 0);
        let keys_read = local_provider(&kek2, 0);

        let data = vec![0x42u8; 20_000];
        let (compressed, _) = compress_with_progress(&data, &[data.len()]).unwrap();
        let blob = encrypt_envelope_segmented(&compressed, 4096, &keys_write).unwrap();

        let (header, _) = crate::v3format::read_header(&blob).unwrap();
        let result = decrypt_envelope_segmented_prefix(&blob, header.segment_count - 1, &keys_read);
        assert!(result.is_err());
    }

    #[test]
    fn segmented_tamper_fails() {
        use crate::compression::compress_with_progress;

        let kek = Kek::generate();
        let keys = local_provider(&kek, 0);

        let data = vec![0x42u8; 20_000];
        let (compressed, _) = compress_with_progress(&data, &[data.len()]).unwrap();
        let mut blob = encrypt_envelope_segmented(&compressed, 4096, &keys).unwrap();

        // Flip a byte inside the first segment's ciphertext (past the
        // header).
        let (header, offset) = crate::v3format::read_header(&blob).unwrap();
        let _ = header;
        blob[offset + 5] ^= 0xFF;

        let result = decrypt_envelope_segmented_prefix(&blob, 0, &keys);
        assert!(result.is_err());
    }

    #[test]
    fn segmented_out_of_range_segment_fails() {
        use crate::compression::compress_with_progress;

        let kek = Kek::generate();
        let keys = local_provider(&kek, 0);

        let data = vec![0x11u8; 5000];
        let (compressed, _) = compress_with_progress(&data, &[data.len()]).unwrap();
        let blob = encrypt_envelope_segmented(&compressed, 4096, &keys).unwrap();

        let (header, _) = crate::v3format::read_header(&blob).unwrap();
        // Ask for one past the last segment.
        let result = decrypt_envelope_segmented_prefix(&blob, header.segment_count, &keys);
        assert!(result.is_err());
    }

    #[test]
    fn segmented_tampered_segment_count_fails_auth() {
        use crate::compression::compress_with_progress;

        let kek = Kek::generate();
        let keys = local_provider(&kek, 0);

        // Incompressible data so the compressed stream spans multiple
        // segments (a repetitive payload would shrink into one segment).
        let mut data = Vec::new();
        let mut state = 0x1234_5678_9abc_def0u64;
        for _ in 0..40_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.push((state & 0xff) as u8);
        }
        let (compressed, _) = compress_with_progress(&data, &[data.len()]).unwrap();
        let mut blob = encrypt_envelope_segmented(&compressed, 4096, &keys).unwrap();

        let (header, _) = crate::v3format::read_header(&blob).unwrap();
        assert!(header.segment_count > 1, "test needs multiple segments");

        // Locate the segment_count field in the header tail and
        // decrement it so the AAD no longer matches the encryption AAD.
        let shared = 4 + 2 + 2 + 4 + header.wrapped_dek.len();
        let sc_offset = shared + 4;
        let sc = u32::from_le_bytes([
            blob[sc_offset],
            blob[sc_offset + 1],
            blob[sc_offset + 2],
            blob[sc_offset + 3],
        ]);
        let tampered_sc = sc - 1;
        blob[sc_offset..sc_offset + 4].copy_from_slice(&tampered_sc.to_le_bytes());

        let (tampered_header, _) = crate::v3format::read_header(&blob).unwrap();
        let result =
            decrypt_envelope_segmented_prefix(&blob, tampered_header.segment_count - 1, &keys);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("authentication failed"),
            "expected authentication failure, got: {}",
            err_msg
        );
    }

    #[test]
    fn segmented_tampered_plaintext_len_fails_auth() {
        use crate::compression::compress_with_progress;

        let kek = Kek::generate();
        let keys = local_provider(&kek, 0);

        let data = vec![0x42u8; 20_000];
        let (compressed, _) = compress_with_progress(&data, &[data.len()]).unwrap();
        let mut blob = encrypt_envelope_segmented(&compressed, 4096, &keys).unwrap();

        // Locate the plaintext_len field in the header tail and bump it
        // so the AAD no longer matches.
        let (header, _) = crate::v3format::read_header(&blob).unwrap();
        let shared = 4 + 2 + 2 + 4 + header.wrapped_dek.len();
        let pl_offset = shared + 8;
        let pl = u64::from_le_bytes([
            blob[pl_offset],
            blob[pl_offset + 1],
            blob[pl_offset + 2],
            blob[pl_offset + 3],
            blob[pl_offset + 4],
            blob[pl_offset + 5],
            blob[pl_offset + 6],
            blob[pl_offset + 7],
        ]);
        let tampered_pl = pl + 1;
        blob[pl_offset..pl_offset + 8].copy_from_slice(&tampered_pl.to_le_bytes());

        let (tampered_header, _) = crate::v3format::read_header(&blob).unwrap();
        let result =
            decrypt_envelope_segmented_prefix(&blob, tampered_header.segment_count - 1, &keys);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("authentication failed"),
            "expected authentication failure, got: {}",
            err_msg
        );
    }

    #[test]
    fn segmented_oversized_plaintext_len_returns_clean_error() {
        use crate::compression::compress_with_progress;
        use crate::v3format;

        let kek = Kek::generate();
        let keys = local_provider(&kek, 0);

        // Build a valid blob, then reconstruct it with an oversized
        // plaintext_len that is still authenticated (AAD matches) so the
        // checked bound -- not the AEAD tag -- is what fires.
        let data = vec![0x42u8; 5000];
        let (compressed, _) = compress_with_progress(&data, &[data.len()]).unwrap();
        let segment_size = 4096usize;
        let real_plaintext_len = compressed.len();

        let (dek, wrapped_dek, kek_version) = keys.wrap_dek().unwrap();
        let segment_count = real_plaintext_len.div_ceil(segment_size).max(1);
        let padded_len = segment_count * segment_size;
        let mut padded = Vec::with_capacity(padded_len);
        padded.extend_from_slice(&compressed);
        padded.resize(padded_len, 0);

        // Use an oversized plaintext_len in both the AAD and the header
        // so segment authentication passes, but the checked bound fires.
        let oversized_len = real_plaintext_len as u64 + 10_000;
        let aad = SegmentAad {
            segment_size: segment_size as u32,
            segment_count: segment_count as u32,
            plaintext_len: oversized_len,
        };

        let mut encrypted_segments = Vec::new();
        for i in 0..segment_count {
            let start = i * segment_size;
            let end = start + segment_size;
            let record = dek
                .encrypt_segment(i as u64, &aad, &padded[start..end])
                .unwrap();
            encrypted_segments.extend_from_slice(&record);
        }

        let mut blob = Vec::new();
        v3format::write_v3(
            &mut blob,
            kek_version,
            &wrapped_dek,
            segment_size as u32,
            segment_count as u32,
            oversized_len,
            &encrypted_segments,
        )
        .unwrap();

        let (header, _) = v3format::read_header(&blob).unwrap();
        let result = decrypt_envelope_segmented_prefix(&blob, header.segment_count - 1, &keys);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeds decrypted segment bytes"),
            "expected oversized-plaintext_len error, got: {}",
            err_msg
        );
    }
}
