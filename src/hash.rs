use multihash::{Code, Multihash, MultihashDigest};

pub fn hash(data: &[u8]) -> Multihash {
    let mh = Code::Sha2_512.digest(&data);
    mh
}
