//! V2 file format for envelope-encrypted blobs and indexes.
//!
//! V2 files use a header containing a wrapped DEK (Data Encryption
//! Key), followed by data encrypted with that DEK using
//! ChaCha20-Poly1305. The DEK is wrapped with the vault's KEK (Key
//! Encryption Key).
//!
//! ## File layout
//!
//! ```text
//! Offset   Size     Field
//! 0        4        Magic: "BLUB" (blob) or "BLUI" (index)
//! 4        2        Format version: 2 (LE u16)
//! 6        2        KEK version (LE u16)
//! 8        4        Wrapped DEK length N (LE u32)
//! 12       N        Wrapped DEK (nonce || ciphertext || tag)
//! 12+N     ...      DEK-encrypted payload
//! ```
//!
//! ## Backward compatibility
//!
//! Files without a recognized magic header are v1 (age-encrypted).
//! The `decrypt_file` function detects the format and dispatches
//! accordingly.

use std::io::{self, Write};

use crate::error::{BluError, Result};
use crate::keys::dek::Dek;
use crate::keys::kek::Kek;

/// Magic bytes for a v2 blob file.
pub const MAGIC_BLOB: [u8; 4] = [0x42, 0x4C, 0x55, 0x42]; // "BLUB"

/// Magic bytes for a v2 index file.
pub const MAGIC_INDEX: [u8; 4] = [0x42, 0x4C, 0x55, 0x49]; // "BLUI"

/// Current format version.
pub const FORMAT_VERSION: u16 = 2;

/// Minimum header size (magic + version + kek_version + wrapped_dek_len).
const HEADER_FIXED_SIZE: usize = 4 + 2 + 2 + 4;

/// The type of v2 file (determines the magic bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// Encrypted blob data file.
    Blob,
    /// Encrypted index file.
    Index,
}

impl FileType {
    /// Return the 4-byte magic for this file type.
    pub fn magic(&self) -> &[u8; 4] {
        match self {
            FileType::Blob => &MAGIC_BLOB,
            FileType::Index => &MAGIC_INDEX,
        }
    }
}

/// A parsed v2 file header.
#[derive(Debug, Clone)]
pub struct V2Header {
    /// The file type (blob or index).
    pub file_type: FileType,
    /// Format version (currently always 2).
    pub format_version: u16,
    /// Which KEK version was used to wrap the DEK.
    pub kek_version: u16,
    /// The wrapped DEK bytes (nonce || ciphertext || tag).
    pub wrapped_dek: Vec<u8>,
}

/// Check whether raw file data starts with a v2 magic header.
pub fn is_v2(data: &[u8]) -> bool {
    if data.len() < 4 {
        return false;
    }
    data[..4] == MAGIC_BLOB || data[..4] == MAGIC_INDEX
}

/// Parse a v2 header from raw file data.
///
/// Returns the header and the offset where the encrypted payload
/// begins.
pub fn read_header(data: &[u8]) -> Result<(V2Header, usize)> {
    if data.len() < HEADER_FIXED_SIZE {
        return Err(BluError::DecryptionFailed(
            "v2 file too short for header".into(),
        ));
    }

    let magic = &data[0..4];
    let file_type = if magic == MAGIC_BLOB {
        FileType::Blob
    } else if magic == MAGIC_INDEX {
        FileType::Index
    } else {
        return Err(BluError::DecryptionFailed(format!(
            "unrecognized magic: {:02x}{:02x}{:02x}{:02x}",
            magic[0], magic[1], magic[2], magic[3]
        )));
    };

    let format_version = u16::from_le_bytes([data[4], data[5]]);
    if format_version != FORMAT_VERSION {
        return Err(BluError::DecryptionFailed(format!(
            "unsupported format version: {} (expected {})",
            format_version, FORMAT_VERSION
        )));
    }

    let kek_version = u16::from_le_bytes([data[6], data[7]]);
    let wrapped_dek_len = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;

    let payload_offset = HEADER_FIXED_SIZE + wrapped_dek_len;
    if data.len() < payload_offset {
        return Err(BluError::DecryptionFailed(format!(
            "v2 file truncated: need {} bytes for header, got {}",
            payload_offset,
            data.len()
        )));
    }

    let wrapped_dek = data[HEADER_FIXED_SIZE..payload_offset].to_vec();

    let header = V2Header {
        file_type,
        format_version,
        kek_version,
        wrapped_dek,
    };

    Ok((header, payload_offset))
}

/// Write a v2 header to a writer.
fn write_header<W: Write>(
    writer: &mut W,
    file_type: FileType,
    kek_version: u16,
    wrapped_dek: &[u8],
) -> io::Result<()> {
    writer.write_all(file_type.magic())?;
    writer.write_all(&FORMAT_VERSION.to_le_bytes())?;
    writer.write_all(&kek_version.to_le_bytes())?;
    writer.write_all(&(wrapped_dek.len() as u32).to_le_bytes())?;
    writer.write_all(wrapped_dek)?;
    Ok(())
}

/// Assemble a v2 file from pre-computed components.
///
/// Used by the Agent path where the DEK wrapping happens via RPC
/// and data encryption happens in-process. Writes the v2 header
/// followed by the already-encrypted payload.
pub fn write_v2<W: Write>(
    writer: &mut W,
    file_type: FileType,
    kek_version: u16,
    wrapped_dek: &[u8],
    encrypted_payload: &[u8],
) -> io::Result<()> {
    write_header(writer, file_type, kek_version, wrapped_dek)?;
    writer.write_all(encrypted_payload)?;
    Ok(())
}

/// Encrypt data in v2 format: generate a DEK, wrap it with the KEK,
/// write the header, and encrypt the payload.
///
/// Returns the complete file contents (header + encrypted payload).
pub fn encrypt_v2(
    data: &[u8],
    kek: &Kek,
    kek_version: u16,
    file_type: FileType,
) -> Result<Vec<u8>> {
    let dek = Dek::generate();
    let wrapped_dek = dek.wrap(kek)?;
    let encrypted_payload = dek.encrypt_data(data)?;

    let total_size = HEADER_FIXED_SIZE + wrapped_dek.len() + encrypted_payload.len();
    let mut output = Vec::with_capacity(total_size);

    write_header(&mut output, file_type, kek_version, &wrapped_dek)
        .map_err(|e| BluError::EncryptionFailed(e.to_string()))?;
    output.extend_from_slice(&encrypted_payload);

    Ok(output)
}

/// Decrypt a v2 file: parse the header, unwrap the DEK with the KEK,
/// and decrypt the payload.
///
/// The caller must provide a function to resolve a KEK by version
/// number, since the file header specifies which KEK version was used.
pub fn decrypt_v2<F>(data: &[u8], kek_resolver: F) -> Result<Vec<u8>>
where
    F: FnOnce(u16) -> Result<Kek>,
{
    let (header, payload_offset) = read_header(data)?;
    let kek = kek_resolver(header.kek_version)?;
    let dek = Dek::unwrap(&kek, &header.wrapped_dek)?;
    let payload = &data[payload_offset..];
    dek.decrypt_data(payload)
}

/// Decrypt file data, auto-detecting v1 (age) or v2 (envelope) format.
///
/// For v2: uses the kek_resolver to get the KEK for the version in
/// the header.
///
/// For v1: falls back to `bbox.decrypt()` (age-based decryption via
/// the BlackBox).
pub fn decrypt_auto<F>(
    data: &[u8],
    bbox: &crate::age::BlackBox,
    kek_resolver: Option<F>,
) -> std::result::Result<Vec<u8>, Box<dyn std::error::Error>>
where
    F: FnOnce(u16) -> Result<Kek>,
{
    if is_v2(data) {
        let resolver = kek_resolver.ok_or_else(|| {
            BluError::DecryptionFailed("v2 file detected but no KEK available".into())
        })?;
        decrypt_v2(data, resolver).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    } else {
        bbox.decrypt(data)
    }
}

/// Encrypt data, choosing v1 or v2 format based on whether a KEK is
/// provided.
///
/// If `kek` is Some, produces v2 format. Otherwise falls back to v1
/// (age via BlackBox).
pub fn encrypt_auto(
    data: &[u8],
    bbox: &crate::age::BlackBox,
    kek: Option<(&Kek, u16)>,
    file_type: FileType,
) -> std::result::Result<Vec<u8>, Box<dyn std::error::Error>> {
    match kek {
        Some((kek, kek_version)) => encrypt_v2(data, kek, kek_version, file_type)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>),
        None => bbox.encrypt(data),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::keys::kek::Kek;

    #[test]
    fn magic_constants() {
        assert_eq!(&MAGIC_BLOB, b"BLUB");
        assert_eq!(&MAGIC_INDEX, b"BLUI");
    }

    #[test]
    fn is_v2_detection() {
        assert!(is_v2(b"BLUB\x02\x00rest"));
        assert!(is_v2(b"BLUI\x02\x00rest"));
        assert!(!is_v2(b"age-encryption.org"));
        assert!(!is_v2(b"BLU"));
        assert!(!is_v2(b""));
    }

    #[test]
    fn encrypt_decrypt_v2_blob() {
        let kek = Kek::generate();
        let plaintext = b"hello v2 blob format";

        let encrypted = encrypt_v2(plaintext, &kek, 0, FileType::Blob).unwrap();
        assert!(is_v2(&encrypted));

        let (header, _) = read_header(&encrypted).unwrap();
        assert_eq!(header.file_type, FileType::Blob);
        assert_eq!(header.format_version, FORMAT_VERSION);
        assert_eq!(header.kek_version, 0);

        let decrypted = decrypt_v2(&encrypted, |v| {
            assert_eq!(v, 0);
            Ok(kek.clone())
        })
        .unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn encrypt_decrypt_v2_index() {
        let kek = Kek::generate();
        let plaintext = b"index data with tags and hashes";

        let encrypted = encrypt_v2(plaintext, &kek, 3, FileType::Index).unwrap();
        assert!(is_v2(&encrypted));

        let (header, _) = read_header(&encrypted).unwrap();
        assert_eq!(header.file_type, FileType::Index);
        assert_eq!(header.kek_version, 3);

        let decrypted = decrypt_v2(&encrypted, |v| {
            assert_eq!(v, 3);
            Ok(kek.clone())
        })
        .unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn decrypt_v2_with_wrong_kek_fails() {
        let kek1 = Kek::generate();
        let kek2 = Kek::generate();

        let encrypted = encrypt_v2(b"secret", &kek1, 0, FileType::Blob).unwrap();
        let result = decrypt_v2(&encrypted, |_| Ok(kek2));
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_v2_truncated_header_fails() {
        let result = read_header(b"BLUB\x02\x00");
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_v2_truncated_wrapped_dek_fails() {
        // Header says wrapped DEK is 100 bytes, but file is too short
        let mut data = Vec::new();
        data.extend_from_slice(b"BLUB");
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&100u32.to_le_bytes());
        // Only 10 bytes of "wrapped DEK" instead of 100
        data.extend_from_slice(&[0u8; 10]);

        let result = read_header(&data);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_v2_bad_version_fails() {
        let mut data = Vec::new();
        data.extend_from_slice(b"BLUB");
        data.extend_from_slice(&99u16.to_le_bytes()); // bad version
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());

        let result = read_header(&data);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_auto_v1_fallback() {
        // Simulate v1: use BlackBox to encrypt, then decrypt_auto should
        // detect non-v2 and fall back to bbox.decrypt()
        let bbox = crate::age::BlackBox::new(&[include_str!("../test/blu_secrets/blu.key")]);
        let plaintext = b"v1 age-encrypted data";

        let encrypted = bbox.encrypt(plaintext).unwrap();
        assert!(!is_v2(&encrypted));

        let decrypted = decrypt_auto::<fn(u16) -> Result<Kek>>(&encrypted, &bbox, None).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn decrypt_auto_v2() {
        let kek = Kek::generate();
        let bbox = crate::age::BlackBox::new(&[include_str!("../test/blu_secrets/blu.key")]);
        let plaintext = b"v2 envelope-encrypted data";

        let encrypted = encrypt_v2(plaintext, &kek, 0, FileType::Blob).unwrap();

        let kek_clone = kek.clone();
        let decrypted = decrypt_auto(&encrypted, &bbox, Some(|_: u16| Ok(kek_clone))).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn encrypt_auto_v2_when_kek_provided() {
        let kek = Kek::generate();
        let bbox = crate::age::BlackBox::new(&[include_str!("../test/blu_secrets/blu.key")]);
        let plaintext = b"auto-encrypt v2";

        let encrypted = encrypt_auto(plaintext, &bbox, Some((&kek, 0)), FileType::Blob).unwrap();
        assert!(is_v2(&encrypted));

        let decrypted = decrypt_v2(&encrypted, |_| Ok(kek.clone())).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn encrypt_auto_v1_when_no_kek() {
        let bbox = crate::age::BlackBox::new(&[include_str!("../test/blu_secrets/blu.key")]);
        let plaintext = b"auto-encrypt v1";

        let encrypted = encrypt_auto(plaintext, &bbox, None, FileType::Blob).unwrap();
        assert!(!is_v2(&encrypted));

        let decrypted = bbox.decrypt(&encrypted).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn header_round_trip() {
        let wrapped_dek = vec![0xAA; 60]; // simulated wrapped DEK
        let mut buf = Vec::new();
        write_header(&mut buf, FileType::Index, 7, &wrapped_dek).unwrap();

        // Append some fake payload
        buf.extend_from_slice(b"payload");

        let (header, offset) = read_header(&buf).unwrap();
        assert_eq!(header.file_type, FileType::Index);
        assert_eq!(header.format_version, FORMAT_VERSION);
        assert_eq!(header.kek_version, 7);
        assert_eq!(header.wrapped_dek, wrapped_dek);
        assert_eq!(&buf[offset..], b"payload");
    }

    #[test]
    fn encrypt_v2_empty_data() {
        let kek = Kek::generate();
        let encrypted = encrypt_v2(b"", &kek, 0, FileType::Blob).unwrap();
        let decrypted = decrypt_v2(&encrypted, |_| Ok(kek.clone())).unwrap();
        assert!(decrypted.is_empty());
    }
}
