use multihash::{Code, Multihash, MultihashDigest};

pub fn hash(data: &[u8]) -> Multihash {
    Code::Sha2_512.digest(data)
}
