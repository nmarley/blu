#![allow(clippy::uninlined_format_args)]

use std::process;

fn main() {
    if let Err(e) = blu::run() {
        eprintln!("Application error: {}", e);
        process::exit(1);
    }
}
