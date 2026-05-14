use std::path::Path;

use crate::block::PlainIndex;
use crate::cli::clapargs::DebugIndexArgs;
use crate::error::BluError;

/// Probably old and should be removed. Debug plain index or something
pub fn debug_index(_args: DebugIndexArgs) -> Result<(), BluError> {
    let dir = Path::new(".");

    let index = PlainIndex::new(dir)?;
    dbg!(&index);

    println!("uniq bytes indexed: {}", index.uniq_bytes_indexed());
    println!("total bytes indexed: {}", index.total_bytes_indexed());
    println!("dupe bytes indexed: {}", index.duplicate_bytes_indexed());

    Ok(())
}
