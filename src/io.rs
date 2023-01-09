use crate::age::BlackBox;
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
