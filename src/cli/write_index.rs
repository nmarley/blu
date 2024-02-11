use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::age::BlackBox;
use crate::block::PlainIndex;
use crate::cli::clapargs::WriteIndexArgs;
use crate::io::BlackBoxSerializable;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// Write the index to a local file
pub async fn write_index(args: WriteIndexArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Started write_index util");

    let dir = Path::new(".");

    let outfile = match args.outfile {
        Some(val) => PathBuf::from(val),
        None => {
            let index_path = Path::new(dir).join(".blu/indexes/index.dat");
            warn!(
                "warn: no outfile given, using default path {}",
                index_path.display()
            );
            index_path
        }
    };

    // test ability to write index file before further processing
    check_outfile_writable(&outfile)?;

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    info!("Indexing {:?}", dir);
    let index = PlainIndex::new(dir)?;

    // back out here since we pass a filename as a direct path
    match write_index_file(&index, &bbox, &outfile) {
        Ok(num_bytes) => info!(
            "Index written to {} ({} bytes)",
            outfile.display(),
            num_bytes
        ),
        Err(e) => error!("Error writing index: {}", e),
    }

    Ok(())
}

pub(crate) async fn check_outfile_writable<P: AsRef<Path>>(
    outfile: P,
) -> Result<(), Box<dyn std::error::Error>> {
    // create parent dir(s) if necessary
    if let Some(parent_dir) = outfile.as_ref().parent() {
        fs::create_dir_all(parent_dir)?;
    }

    fs::File::create(&outfile).map_err(|e| -> Box<dyn std::error::Error> {
        format!(
            "unable to write to outfile '{}': {}",
            outfile.as_ref().display(),
            e
        )
        .into()
    })?;

    Ok(())
}

pub(crate) async fn write_index_file<P: AsRef<Path>>(
    index: &PlainIndex,
    bbox: &BlackBox,
    outfile: P,
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut enc_idx_bytes = Vec::new();
    index.write(&mut enc_idx_bytes, bbox)?;
    let size = enc_idx_bytes.len();
    let mut file = fs::File::create(outfile).await?;
    file.write_all(&enc_idx_bytes).await?;
    Ok(size)
}
