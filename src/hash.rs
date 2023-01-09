use multihash::{Code, Multihash, MultihashDigest};
use serde::{Deserialize, Serialize};

/// Returns a multihash of the given data. Currently uses the sha-512 hash.
///
/// This is a bad design and should be more flexible in the hash to be used.
pub fn multihash(data: &[u8]) -> Multihash {
    Code::Sha2_512.digest(data)
}

/// Hash is a Vec<u8> type alias with syntactic sugar to allow easier debugging
/// as a hex string.
#[derive(Serialize, Deserialize, PartialEq, Clone, Hash, Eq, Ord, PartialOrd)]
pub struct Hash(Vec<u8>);
impl std::fmt::Debug for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = write!(f, "{:?}", &hex::encode(&self.0));
        Ok(())
    }
}
impl From<Vec<u8>> for Hash {
    fn from(vec: Vec<u8>) -> Self {
        Self(vec)
    }
}
impl From<&[u8]> for Hash {
    fn from(slice: &[u8]) -> Self {
        Self(slice.to_owned())
    }
}
impl From<&str> for Hash {
    fn from(str_ref: &str) -> Self {
        Self(hex::decode(str_ref).unwrap())
    }
}
impl Hash {
    /// Returns the bytes which constitute the multihash.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}
