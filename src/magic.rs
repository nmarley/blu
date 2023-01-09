use filemagic::Magic;

const MAGIC_BUF_MAX_SIZE: usize = 1024;
const BREW_MAGIC_DB: &str = "/usr/local/share/misc/magic.mgc";

/// A wrapper around the filemagic crate. This is used to determine the file
/// type.
pub struct Wizard {
    magic: Magic,
}

impl Wizard {
    /// Create a new Wizard.
    pub fn new() -> Wizard {
        let m = Magic::open(Default::default()).unwrap();
        let magic_dbs = vec![BREW_MAGIC_DB];
        m.load(&magic_dbs).unwrap();

        Wizard { magic: m }
    }

    /// Get the file type of the given data.
    pub fn get_filetype(
        &self,
        data: &[u8],
        size: usize,
    ) -> Result<String, filemagic::FileMagicError> {
        let magic_vec_capacity = if size < MAGIC_BUF_MAX_SIZE {
            size
        } else {
            MAGIC_BUF_MAX_SIZE
        };
        let mut magic_buf = vec![0; magic_vec_capacity];
        let _ = &magic_buf[0..magic_vec_capacity].copy_from_slice(&data[0..magic_vec_capacity]);

        self.magic.buffer(&magic_buf)
    }
}

impl Default for Wizard {
    fn default() -> Self {
        Self::new()
    }
}
