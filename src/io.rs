use crate::age::BlackBox;
use serde::{Deserialize, Serialize};
use std::io;

/// BlackBoxSerializable is a trait for serializing and deserializing structs to
/// and from a stream. In theory the methods should implement compression and
/// encryption as well.
pub trait BlackBoxSerializable {
    /// write the struct to the given stream
    fn write<W: io::Write>(
        &self,
        stream: W,
        bbox: &BlackBox,
    ) -> Result<(), Box<dyn std::error::Error>>;
    /// read the struct from the given stream
    fn read<R: io::Read>(stream: R, bbox: &BlackBox) -> Result<Self, Box<dyn std::error::Error>>
    where
        Self: Sized;
    /// deserialize the struct from a byte vector
    fn deserialize_bytes(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>>
    where
        Self: Sized;
    /// serialize the struct to a byte vector
    fn serialize_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>>;
}

/// macro to generate standard implementation of BlackBoxSerializable
macro_rules! gen_std_bbserde {
    ($struct_name:ident) => {
        impl BlackBoxSerializable for $struct_name {
            fn write<W: io::Write>(
                &self,
                mut stream: W,
                bbox: &BlackBox,
            ) -> Result<(), Box<dyn std::error::Error>> {
                let serialized = self.serialize_bytes()?;
                let compressed = compress(&serialized)?;
                let encrypted = bbox.encrypt_index(&compressed)?;
                let _ = stream.write_all(&encrypted);
                Ok(())
            }

            fn deserialize_bytes(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
                // let decoded: Index = serde_cbor::from_slice(data)?;
                let decoded: Self = bincode::deserialize(data)?;
                Ok(decoded)
            }

            fn serialize_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
                let encoded: Vec<u8> = bincode::serialize(&self)?;
                // let encoded: Vec<u8> = serde_cbor::to_vec(&self)?;
                Ok(encoded)
            }

            // read / write serialization methods integrate BlackBox for automagic
            // also compress and decompress
            fn read<R: io::Read>(
                mut stream: R,
                bbox: &BlackBox,
            ) -> Result<Self, Box<dyn std::error::Error>> {
                let mut encrypted = Vec::new();
                let _ = stream.read_to_end(&mut encrypted)?;
                let compressed = bbox.decrypt(&encrypted)?;
                let serialized = decompress(&compressed)?;
                Self::deserialize_bytes(&serialized)
            }
        }
    };
}

pub(crate) use gen_std_bbserde;

/// Position is the offset and size of a chunk of data within a bigger data
/// block.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Eq)]
pub struct Position {
    /// Offset is where to start reading
    pub offset: usize,
    /// Size is how many bytes to read
    pub size: usize,
}
