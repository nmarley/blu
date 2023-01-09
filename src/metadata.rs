// note: this module is deprecated and I don't want to waste time generating docs for it
#![allow(missing_docs)]

mod encrypted;
mod entry;
mod index;

pub use encrypted::{Encrypted, EncryptedIndex};
pub use index::{Index, INDEX_FILENAME};
