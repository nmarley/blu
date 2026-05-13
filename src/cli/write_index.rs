use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::block::PlainIndex;
use crate::cli::clapargs::WriteIndexArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::dek_provider::DekProvider;
use crate::io::EncryptedSerializable;

/// Write the index to a local file
pub fn write_index(args: WriteIndexArgs) -> Result<(), Box<dyn std::error::Error>> {
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

    let (_cfg, keys) = load_config_and_keys(&LoadOptions::default())?;
    info!("Indexing {:?}", dir);
    let index = PlainIndex::new(dir)?;

    // back out here since we pass a filename as a direct path
    match write_index_file(&index, &keys, &outfile) {
        Ok(num_bytes) => info!(
            "Index written to {} ({} bytes)",
            outfile.display(),
            num_bytes
        ),
        Err(e) => error!("Error writing index: {}", e),
    }

    Ok(())
}

pub(crate) fn check_outfile_writable<P: AsRef<Path>>(
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

pub(crate) fn write_index_file<P: AsRef<Path>>(
    index: &PlainIndex,
    keys: &DekProvider,
    outfile: P,
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut enc_idx_bytes = Vec::new();
    index.write(&mut enc_idx_bytes, keys)?;
    let size = enc_idx_bytes.len();
    let mut file = fs::File::create(outfile)?;
    file.write_all(&enc_idx_bytes)?;
    Ok(size)
}
