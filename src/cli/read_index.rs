use std::fs;

use crate::io::EncryptedSerializable;

// index types
use crate::blob::BlobIndex;
use crate::block::PlainIndex;
use crate::tag::TagIndex;

use crate::cli::clapargs::{IndexType, ReadIndexArgs};
use crate::cli::helpers::{load_config_and_keys, LoadOptions};

/// Read and print individual index files, for debugging
pub fn read_index(args: ReadIndexArgs) -> Result<(), Box<dyn std::error::Error>> {
    let index_file = &args.file;

    let (_cfg, keys) = load_config_and_keys(&LoadOptions::default())?;
    let data = fs::read(index_file)?;

    match args.index_type {
        IndexType::Plain => {
            let index = PlainIndex::read(&data[..], &keys)?;
            dbg!(&index);
        }
        IndexType::Blob => {
            let index = BlobIndex::read(&data[..], &keys)?;
            dbg!(&index);
        }
        IndexType::Tag => {
            let index = TagIndex::read(&data[..], &keys)?;
            dbg!(&index);
        }
    }

    Ok(())
}
