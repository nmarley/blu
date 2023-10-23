use std::env;
use std::path::Path;

use crate::block::PlainIndex;
use crate::cli::clapargs::DebugIndexArgs;

/// Probably old and should be removed. Debug plain index or something
pub fn debug_index(args: DebugIndexArgs) -> Result<(), Box<dyn std::error::Error>> {
    // move into the basedir for all operations, like `git -C <dir>`
    let basedir = args.dir;
    env::set_current_dir(basedir)?;
    let index_dir = Path::new(".");

    let index = PlainIndex::new(index_dir)?;
    dbg!(&index);
    println!("uniq bytes indexed: {}", index.uniq_bytes_indexed());
    println!("total bytes indexed: {}", index.total_bytes_indexed());
    println!("dupe bytes indexed: {}", index.duplicate_bytes_indexed());

    Ok(())
}
