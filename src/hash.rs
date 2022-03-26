use multihash::{Code, Multihash, MultihashDigest};
use serde::{Deserialize, Serialize};

pub fn multihash(data: &[u8]) -> Multihash {
    Code::Sha2_512.digest(data)
}

// all this to debug the Vec<u8> as a hex string instead of numbers
#[derive(Serialize, Deserialize, PartialEq, Clone, Hash, Eq, Ord, PartialOrd)]
pub struct MyHash(Vec<u8>);
impl std::fmt::Debug for MyHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = write!(f, "{:?}", &hex::encode(&self.0));
        Ok(())
    }
}
impl From<Vec<u8>> for MyHash {
    fn from(vec: Vec<u8>) -> Self {
        Self(vec)
    }
}
impl From<&[u8]> for MyHash {
    fn from(slice: &[u8]) -> Self {
        Self(slice.to_owned())
    }
}
impl From<&str> for MyHash {
    fn from(str_ref: &str) -> Self {
        Self(hex::decode(str_ref).unwrap())
    }
}
impl MyHash {
    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}
