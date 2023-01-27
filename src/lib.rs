#![warn(rust_2018_idioms)]
// #![warn(missing_debug_implementations)]
#![warn(missing_docs)]
//
// https://doc.rust-lang.org/rustc/lints/groups.html
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::needless_lifetimes)]

//! Blu is an encrypted and de-duplicated file archival system.
//!
//! > "Not your keys, not your secrets ..."
//!
//! Based on directories in the typical \*nix hierarchical file system (HFS), this will read all
//! files in the directory, and encrypt, de-duplicate and archive to any of several configurable
//! backends, including locally and cloud object storage such as Amazon s3.
//!
//! All encryption in the project uses [rage](https://github.com/str4d/rage), based on age by
//! [@FiloSottile](https://twitter.com/FiloSottile) and
//! [@Benjojo12](https://twitter.com/Benjojo12).

use clap::Parser;
use std::{env, process};

#[macro_use]
extern crate log;

/// age handles all encryption and decryption
pub mod age;
/// blob handles storage and retrieval of encrypted files
pub mod blob;
/// block handles block-based indexing
pub mod block;
/// clap handles command line parsing
pub mod clapargs;
/// helper functions for (de+)compression
pub mod compression;
/// configuration file and related methods
pub mod config;
/// dir probably needs to be moved to blobbuffer / encrypted storage
pub mod dir;
/// format contains a format fn for datetime (chrono/serde)
pub mod format;
/// wrapper around Vec<u8> for cryptographic hashes
pub mod hash;
/// serialization + compression + encryption for indexes
pub mod io;
/// file magic helper
pub mod magic;
/// old file-based indexing, deprecated
pub mod metadata;
/// tag index, probably should rename this
pub mod tagger;

/// cli interface, very immature, needs to be reworked
pub mod cmds;

use crate::age::BlackBox;

const TEST_AGE_SECRET_KEY: &str = include_str!("../test/blu_secrets/blu.key");

// also: consider an internal webserver which serves up the UI for blu

/// run the main 'blu' binary. Not sure this makes sense to exist anymore.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: clapargs::Args = clapargs::Args::parse();

    // TODO: use pwd
    let dir = env::var("BLU_DIR").unwrap_or_else(|_| {
        println!("error: required env var BLU_DIR is not set");
        process::exit(1);
    });
    dbg!(&dir);

    let cfg = config::read_config(&dir)?;
    dbg!(&cfg);

    let bbox = load_key();
    dbg!(&bbox);
    // let mut index = match cfg.load_plain_index(&bbox) {
    //     None => Index::new(&dir)?,
    //     Some(idx) => idx,
    // };
    // let mut index = Index::new(dir)?;

    match args.action {
        // There are 2 basic operations:
        //     a. archive - encrypt+de-duplicate new files
        //     b. restore - restore from backup
        //
        clapargs::Action::Add => {
            cmds::add();
        }
        clapargs::Action::Init => {
            cmds::init();
        }
        clapargs::Action::Restore => {
            cmds::restore();
        }
        clapargs::Action::ListTags => {
            cmds::list_tags(&cfg, &bbox);
        }
        _ => {
            unimplemented!();
        }
    };

    Ok(())
}

// TODO: Rename/multi keys
fn load_key() -> BlackBox {
    BlackBox::new(&[TEST_AGE_SECRET_KEY])
}
