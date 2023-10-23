use std::fs;

// encryption/serialization stuff
use crate::age::BlackBox;
use crate::io::BlackBoxSerializable;

// index types
use crate::blob::BlobIndex;
use crate::block::PlainIndex;
use crate::tag::TagIndex;

use crate::cli::clapargs::{IndexType, ReadIndexArgs};

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// Read and print individual index files, for debugging
pub fn read_index(args: ReadIndexArgs) -> Result<(), Box<dyn std::error::Error>> {
    let index_file = &args.file;

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
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
