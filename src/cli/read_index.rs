use std::fs;

use crate::io::BlackBoxSerializable;

// index types
use crate::blob::BlobIndex;
use crate::block::PlainIndex;
use crate::tag::TagIndex;

use crate::cli::clapargs::{IndexType, ReadIndexArgs};
use crate::cli::helpers::{load_config_and_blackbox, LoadOptions};

/// Read and print individual index files, for debugging
pub fn read_index(args: ReadIndexArgs) -> Result<(), Box<dyn std::error::Error>> {
    let index_file = &args.file;

    let (_cfg, bbox) = load_config_and_blackbox(&LoadOptions::default())?;
    let data = fs::read(index_file)?;

    match args.index_type {
        IndexType::Plain => {
            let index = PlainIndex::read(&data[..], &bbox)?;
            dbg!(&index);
        }
        IndexType::Blob => {
            let index = BlobIndex::read(&data[..], &bbox)?;
            dbg!(&index);
        }
        IndexType::Tag => {
            let index = TagIndex::read(&data[..], &bbox)?;
            dbg!(&index);
        }
    }

    Ok(())
}
