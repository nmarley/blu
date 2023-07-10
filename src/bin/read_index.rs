#![allow(clippy::uninlined_format_args)]

use clap::Parser;
use std::fs;

// encryption/serialization stuff
use blu::age::BlackBox;
use blu::io::BlackBoxSerializable;

// index types
use blu::blob::BlobIndex;
use blu::block::PlainIndex;
use blu::tag::TagIndex;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

#[derive(Parser)]
pub struct Args {
    #[clap(value_enum)]
    pub index_type: IndexType,
    pub file: String,
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum IndexType {
    Plain,
    Blob,
    Tag,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
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
