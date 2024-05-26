use std::path::Path;
use tokio::fs;

use crate::age::BlackBox;
use crate::blob::BlobIndex;
use crate::cli::clapargs::DefragBlobsArgs;
use crate::io::BlackBoxSerializable;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// Defrag blobs is still a WIP
pub async fn defrag_blobs(args: DefragBlobsArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Started defrag_blobs util");

    // move into the basedir for all operations, like `git -C <dir>`
    // env::set_current_dir(args.dir)?;
    // let dir = Path::new(".");

    info!("blob_index_path: {}", args.blob_index_path);

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);

    let blob_index = load_blob_index(&bbox, args.blob_index_path).await.unwrap();
    info!(
        "Blob index has {} blob files",
        blob_index.count_blob_files()
    );

    // Now we've hit the classic Bin Packing problem:
    // https://en.wikipedia.org/wiki/Bin_packing_problem
    //
    // Let's just use the First-Fit Decreasing (FFD) algorithm and call it good
    // (enough).

    // let backend = cfg.init_storage_backend().await?;

    for (blob_path, set_chunk_hashes) in blob_index.path_index.iter() {
        let mut blob_size = 0_usize;
        for chunk_hash in set_chunk_hashes.iter() {
            blob_size += blob_index.map.get(chunk_hash).unwrap().position.size;
        }
        info!(
            "Got blob path: {} with {} hashes, {} total bytes",
            blob_path.display(),
            set_chunk_hashes.len(),
            blob_size,
        );
    }

    if !args.dry_run {
        info!("do something here");
    } else {
        info!("Got dry_run flag, will not write");
    }

    Ok(())
}

async fn load_blob_index<P: AsRef<Path>>(bbox: &BlackBox, index_path: P) -> Option<BlobIndex> {
    // read index file data or return None
    let index_data: Vec<u8> = fs::read(index_path.as_ref()).await.ok()?;
    // deserialize + decompress + decrypt index or return None
    BlobIndex::read(&index_data[..], bbox).ok()
}
