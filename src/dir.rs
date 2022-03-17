use crate::hash;
use crate::metadata::Encrypted;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Default, Debug)]
pub struct Manager {
    datadir: PathBuf,
}
use multihash::Multihash;

impl Manager {
    pub fn new<P: AsRef<Path> + std::fmt::Debug>(datadir: P) -> Self {
        Self {
            datadir: datadir.as_ref().to_path_buf(),
        }
    }

    // If this worked, caller should remove the Encrypted and make it a None in
    // the index...  Sadly we can't know if it worked, so... I guess just assume
    // it worked.
    pub fn delete_encrypted(&self, enc: &Encrypted) -> Result<(), Box<dyn std::error::Error>> {
        let path = self.path_for(&enc.get_hash())?;
        fs::remove_file(path)?;
        Ok(())
    }

    pub fn write_encrypted(&self, data: &[u8]) -> Result<Encrypted, Box<dyn std::error::Error>> {
        // if this worked, caller should replace the None in the index w/an Encrypted

        let mh = hash::hash(data);
        let hash = mh.to_bytes();

        let path = self.path_for(&hash)?;

        let size = hash.len() as u64;

        // create the path (directories)
        fs::create_dir_all(path.parent().unwrap())?;
        // write file
        fs::write(&path, &data)?;

        // hash data
        let enc = Encrypted {
            path,
            hash,
            size,
            keys: vec![],
        };
        Ok(enc)
    }

    // get a path for the encrypted
    // this is generally the hash, but broken into a dir structure
    // also with the multihash prefix(es) removed from the front...
    //
    // example, this hash ... :
    // 1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
    //
    // ... would be stored in:
    // DATADIR / d / dd4 / dd4ce / dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
    //
    fn path_for(&self, hash: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>> {
        // use multihash lib to properly separate multihash header code and size
        // (do not make assumptions about removing X number of bytes)

        let mh = Multihash::from_bytes(hash)?;
        // dbg!(&mh.code());
        // dbg!(&mh.size());
        // dbg!(&mh.digest());

        let hash_str = hex::encode(&mh.digest());
        // dbg!(&hash_str);

        Ok(self
            .datadir
            .join(&hash_str[0..1])
            .join(&hash_str[0..3])
            .join(&hash_str[0..5])
            .join(&hash_str))
    }
}

#[cfg(test)]
mod test {
    use super::Manager;
    use std::path::PathBuf;

    // TODO: macro w/several different versions of this ... can use different
    // multihashes too, to test that
    macro_rules! test_path_for {
        ($name:ident, $hash:expr, $path:expr) => {
            #[test]
            fn $name() {
                let dir_mgr = Manager::new("/tmp");
                let hash = hex::decode($hash).unwrap();
                let path = dir_mgr.path_for(&hash).unwrap();
                assert_eq!(path, PathBuf::from($path));
            }
        };
    }

    // DATADIR / d / dd4 / dd4ce38e / dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6
    test_path_for!(
        path_for_sha2_512,
        "1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6",
        "/tmp/d/dd4/dd4ce/dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6"
    );
    test_path_for!(
        path_for_sha2_256,
        "12209b2f4374822ae5b8a14e89f69bdcc1b570948e201f318c763ee1c31d2fb02f3d",
        "/tmp/9/9b2/9b2f4/9b2f4374822ae5b8a14e89f69bdcc1b570948e201f318c763ee1c31d2fb02f3d"
    );
    test_path_for!(
        path_for_sha3_256,
        "16202a62db58c655ef1484f5c5d8bbd8eb9b75261a149db76b9e0177831325f5030e",
        "/tmp/2/2a6/2a62d/2a62db58c655ef1484f5c5d8bbd8eb9b75261a149db76b9e0177831325f5030e"
    );
    test_path_for!(
        path_for_blake2b_256,
        "a0e4022064982f9ad98dc4845638d6ed1abc2ef2f76d90eecc9091e4802e73734b96ec36",
        "/tmp/6/649/64982/64982f9ad98dc4845638d6ed1abc2ef2f76d90eecc9091e4802e73734b96ec36"
    );
}
