use crate::age::BlackBox;
use std::io;

pub trait BlackBoxSerializable {
    fn write<W: io::Write>(
        &self,
        stream: W,
        bbox: &BlackBox,
    ) -> Result<(), Box<dyn std::error::Error>>;
    fn read<R: io::Read>(stream: R, bbox: &BlackBox) -> Result<Self, Box<dyn std::error::Error>>
    where
        Self: Sized;
    fn deserialize_bytes(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>>
    where
        Self: Sized;
    fn serialize_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>>;
}
