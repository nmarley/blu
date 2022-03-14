use crate::metadata::Encrypted;
use std::path::{Path, PathBuf};

#[derive(Default, Debug)]
pub struct Manager {
    datadir: PathBuf,
}

impl Manager {
    pub fn new<P: AsRef<Path> + std::fmt::Debug>(datadir: P) -> Self {
        Self {
            datadir: datadir.as_ref().to_path_buf(),
        }
    }

    pub fn delete_encrypted(&self, enc: &Encrypted) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    pub fn write_encrypted(
        &self,
        enc: &Encrypted,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    // get a path for the encrypted
    // this is generally the hash, but broken into a dir structure
    // also with the multihash prefix(es) removed from the front...
    //
    // example, this hash ... :
    // 1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
    //
    // ... would be stored in:
    // DATADIR / d / dd4 / dd4ce38e / dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
    //
    // TODO: makes sure this is actually an ABS path, otherwise rename
    fn abs_path_for(&self, hash: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>> {
        // TODO: use multihash lib to properly remove MH header bytes (do not
        // make assumptions about removing X number of bytes)

        // https://docs.rs/multihash/0.16.1/multihash/struct.MultihashGeneric.html#example
        //
        // use multihash::Multihash;
        //
        // const Sha3_256: u64 = 0x16;
        // let digest_bytes = [
        //     0x16, 0x20, 0x64, 0x4b, 0xcc, 0x7e, 0x56, 0x43, 0x73, 0x04, 0x09, 0x99, 0xaa, 0xc8, 0x9e,
        //     0x76, 0x22, 0xf3, 0xca, 0x71, 0xfb, 0xa1, 0xd9, 0x72, 0xfd, 0x94, 0xa3, 0x1c, 0x3b, 0xfb,
        //     0xf2, 0x4e, 0x39, 0x38,
        // ];
        // let mh = Multihash::from_bytes(&digest_bytes).unwrap();
        // assert_eq!(mh.code(), Sha3_256);
        // assert_eq!(mh.size(), 32);
        // assert_eq!(mh.digest(), &digest_bytes[2..]);
        dbg!(&hash);

        Ok(self.datadir.join("").into())
    }
}

#[cfg(test)]
mod test {
    use super::Manager;
    use std::path::PathBuf;

    // TODO: macro w/several different versions of this ... can use different
    // multihashes too, to test that
    #[test]
    fn abs_path_for() {
        let dir_mgr = Manager::new("/tmp");
        let hash = hex::decode("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6").unwrap();

        // DATADIR / d / dd4 / dd4ce38e / dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
        let abs_path = dir_mgr.abs_path_for(&hash).unwrap();
        assert_eq!(abs_path, PathBuf::from("/tmp/d/dd4/dd4ce38e/dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6"));
    }
}
