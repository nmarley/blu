use crate::dek_provider::DekProvider;
use serde::{Deserialize, Serialize};
use std::io;

/// Trait for types that can be serialized, compressed, encrypted, and
/// written to a stream (and the reverse for reading).
pub trait EncryptedSerializable {
    /// Serialize, compress, encrypt, and write to the given stream.
    fn write<W: io::Write>(
        &self,
        stream: W,
        keys: &DekProvider,
    ) -> Result<(), Box<dyn std::error::Error>>;
    /// Read, decrypt, decompress, and deserialize from the given stream.
    fn read<R: io::Read>(stream: R, keys: &DekProvider) -> Result<Self, Box<dyn std::error::Error>>
    where
        Self: Sized;
    /// Deserialize from raw (unencrypted, uncompressed) bytes.
    fn deserialize_bytes(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>>
    where
        Self: Sized;
    /// Serialize to raw (unencrypted, uncompressed) bytes.
    fn serialize_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>>;
}

/// Macro to generate the standard `EncryptedSerializable` implementation
/// for a type that derives `Serialize` and `Deserialize`.
macro_rules! gen_std_enc_serde {
    ($struct_name:ident) => {
        impl crate::io::EncryptedSerializable for $struct_name {
            fn write<W: io::Write>(
                &self,
                mut stream: W,
                keys: &crate::dek_provider::DekProvider,
            ) -> Result<(), Box<dyn std::error::Error>> {
                let serialized = self.serialize_bytes()?;
                let compressed = compress(&serialized)?;
                let encrypted = crate::dek_provider::encrypt_envelope(
                    &compressed,
                    crate::v2format::FileType::Index,
                    keys,
                )?;
                let _ = stream.write_all(&encrypted);
                Ok(())
            }

            fn deserialize_bytes(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
                let decoded: Self = bincode::deserialize(data)?;
                Ok(decoded)
            }

            fn serialize_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
                let encoded: Vec<u8> = bincode::serialize(&self)?;
                Ok(encoded)
            }

            fn read<R: io::Read>(
                mut stream: R,
                keys: &crate::dek_provider::DekProvider,
            ) -> Result<Self, Box<dyn std::error::Error>> {
                let mut encrypted = Vec::new();
                let _ = stream.read_to_end(&mut encrypted)?;
                let compressed = crate::dek_provider::decrypt_envelope(&encrypted, keys)?;
                let serialized = decompress(&compressed)?;
                Self::deserialize_bytes(&serialized)
            }
        }
    };
}

pub(crate) use gen_std_enc_serde;

/// Position is the offset and size of a chunk of data within a bigger data
/// block.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Eq)]
pub struct Position {
    /// Offset is where to start reading
    pub offset: usize,
    /// Size is how many bytes to read
    pub size: usize,
}
