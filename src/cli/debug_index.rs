use std::path::Path;

use crate::block::PlainIndex;
use crate::cli::clapargs::DebugIndexArgs;

/// Probably old and should be removed. Debug plain index or something
pub async fn debug_index(_args: DebugIndexArgs) -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(".");

    let index = PlainIndex::new(dir).await?;
    dbg!(&index);

    println!("uniq bytes indexed: {}", index.uniq_bytes_indexed());
    println!("total bytes indexed: {}", index.total_bytes_indexed());
    println!("dupe bytes indexed: {}", index.duplicate_bytes_indexed());

    Ok(())
}
