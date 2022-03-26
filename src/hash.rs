use multihash::{Code, Multihash, MultihashDigest};

pub fn multihash(data: &[u8]) -> Multihash {
    Code::Sha2_512.digest(data)
}
