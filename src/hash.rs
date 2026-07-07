use multihash::Multihash;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};

// See:
// https://github.com/multiformats/multicodec/blob/master/table.csv
pub(crate) const SHA2_512: u64 = 0x13;

/// Returns a multihash of the given data. Currently uses the sha-512 hash.
///
/// This is a bad design and should be more flexible in the hash to be used.
pub fn multihash(data: &[u8]) -> Multihash<64> {
    let digest_bytes = sha512(data);

    Multihash::wrap(SHA2_512, &digest_bytes).unwrap()
}

/// Hash is a Vec<u8> type alias with syntactic sugar to allow easier debugging
/// as a hex string.
#[derive(Serialize, Deserialize, PartialEq, Clone, Hash, Eq, Ord, PartialOrd)]
pub struct Hash(Vec<u8>);
impl std::fmt::Debug for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // use multihash lib to properly separate multihash header code and size
        // (do not make assumptions about removing X number of bytes)
        //
        // TODO: re-implement how we store the multihash in the Hash type, or
        // just alias to MultiHash w/some syntactic sugar methods
        match Multihash::<64>::from_bytes(&self.0) {
            Ok(mh) => write!(
                f,
                "Hash {{ code: {}, digest: {} }}",
                mh.code(),
                &hex::encode(mh.digest())
            ),
            Err(_) => write!(f, "Hash {{ raw: {} }}", hex::encode(&self.0)),
        }
    }
}

impl std::fmt::Display for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = write!(f, "{}", &hex::encode(&self.0));
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

    /// Return a short version of hash in hex
    pub fn dbg_short(&self, len: usize) -> String {
        match Multihash::<64>::from_bytes(&self.0) {
            Ok(mh) => hex::encode(mh.digest())
                .chars()
                .take(len)
                .collect::<String>(),
            Err(_) => hex::encode(&self.0).chars().take(len).collect::<String>(),
        }
    }
}

pub(crate) fn sha512(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha512::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Incremental SHA-512 multihash for streaming data that does not fit
/// comfortably in memory.
///
/// Feed data via [`StreamingHash::update`] as it arrives (over HTTP,
/// from a temp file, etc.), then call [`StreamingHash::finalize`] to
/// obtain the same `Hash` that [`multihash`] would produce for the
/// concatenated bytes. Used by the `blu serve` streaming PUT path so
/// the whole-file hash is computed without buffering the entire body.
pub struct StreamingHash {
    hasher: Sha512,
}

impl StreamingHash {
    /// Create a new empty incremental hasher.
    pub fn new() -> Self {
        Self {
            hasher: Sha512::new(),
        }
    }

    /// Feed a chunk of data into the running hash.
    pub fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }

    /// Finalize the hash and return the multihash-encoded `Hash`,
    /// matching what [`multihash`] would return for the full
    /// concatenated input.
    pub fn finalize(self) -> Hash {
        let digest = self.hasher.finalize();
        let mh: Multihash<64> = Multihash::wrap(SHA2_512, &digest).unwrap();
        Hash::from(mh.to_bytes())
    }

    /// Finalize the hash and return the raw SHA-512 digest without
    /// the multihash envelope. Used where the caller needs the raw
    /// digest (e.g., part ETags in multipart upload).
    pub fn finalize_raw(self) -> Vec<u8> {
        self.hasher.finalize().to_vec()
    }
}

impl Default for StreamingHash {
    fn default() -> Self {
        Self::new()
    }
}
