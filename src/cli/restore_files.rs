use std::collections::HashSet;
use std::io::SeekFrom;
use std::path::Path;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::age::BlackBox;
use crate::blob::EncBlobReader;
use crate::cli::clapargs::RestoreFilesArgs;
use crate::config;
use crate::hash::Hash;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// Restore plain-text files from the archive, requires index + necessary encrypted blobs
pub async fn restore_files(args: RestoreFilesArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Started restore_files util");
    info!("Got file_hashes: {:?}", args.file_hashes);

    let dir = Path::new(".");

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);

    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;
    let plain_index = cfg.load_plain_index(&bbox).unwrap();
    let blob_index = cfg.load_blob_index(&bbox).unwrap_or_default();
    let files_map = plain_index.files_map_ref();

    let backend = cfg.init_storage_backend()?;

    // NOTE:
    //     `*` derefs the `Box<dyn StorageBackend>`
    //     BlobBuffer::new expects a `&dyn StorageBackend`
    let mut reader = EncBlobReader::new(&bbox, &(*backend));

    // info!("Got file_hashes: {:?}", args.file_hashes);
    let mut unique_hashes: HashSet<Hash> = HashSet::new();
    // TODO: consider disambiguating hash filters if a short hash prefix might
    // identify multiple files, sorta like git does
    for hash in files_map.keys() {
        // in theory the provided file hash list will be smaller than the number
        // of entries in the index
        for h in &args.file_hashes {
            // TODO: better than this.
            if hash.to_string().contains(h) {
                println!("Got a match on file hash: {}", hash.dbg_short(9));
                unique_hashes.insert(hash.clone());
            }
        }
    }

    'outer: for file_hash in unique_hashes.into_iter() {
        println!("========================================================================");
        println!("Restoring file: {:?}", file_hash);
        let fileref = match plain_index.get_fileref_ref(&file_hash) {
            Some(fileref) => fileref,
            None => {
                eprintln!(
                    "Unable to restore file: File hash not found in plain index: {:?}",
                    file_hash
                );
                continue; // next file
            }
        };

        let file_size = fileref.total_size();
        println!("Size: {}", file_size);
        println!("Filename(s):");

        // TODO: consider multiple paths... what to do if the same path exists
        // with different filenames?  This might be a UX concern also.

        // check each file path and abort if there is a collision
        for path in fileref.paths.iter() {
            println!("\t{:?}", path);
            // abort if file exists with this filename
            if std::path::Path::exists(path) {
                eprintln!("Unable to restore file: There already exists in the filesystem a file at the path: {:?}", path);
                continue 'outer; // next file
            }
        }

        // TODO: hard links for the same data with multiple filenames

        // TODO: restore to a temp working dir and do a filesystem rename
        // instead of creating the destination file directly

        // after file restored to `restore_path`, create FS hard links to
        // `other_paths`
        let mut path_iter = fileref.paths.iter();
        let restore_path = path_iter.next().unwrap();
        let other_paths = path_iter.collect::<Vec<_>>();
        println!(
            "Choosing path: '{}' for restoration",
            restore_path.display()
        );

        // TODO: Create the TEMP file first. If restore fails at any
        // data block, fail and move on, and remove the temp file.

        // 2. Next, create a sparse file of the correct size and start to fill in the gaps
        //    from our block data (decrypted the encrypted blobs).
        // create sparse file that's X bytes long
        println!("Creating sparse file of size: {}", file_size);
        let mut fh = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(restore_path)
            .await?;
        let _ = fh
            .set_len(file_size)
            .await
            .map_err(|e| eprintln!("Unable to set length of new sparse file: {:?}", e));

        let mut offset = 0u64;
        // slowness here ...
        for chunkmeta in fileref.chunkmetas.iter() {
            if !blob_index.has_chunk(&chunkmeta.hash) {
                // abort restore of this file, remove TEMP file and move on to next ...
                //
                // TODO: maybe don't abort (esp. for large files which would
                // piss ppl off), and instead just write the other chunks and
                // log the ones not found in the blob index. The files would be
                // corrupted / not intact so we should report it, but could
                // ostensibly be fixed w/some repair tool if the blobs can be
                // found later.
                eprintln!("Unable to restore file: Block hash not found in blob index for block: {:?}, file: {:?}", chunkmeta.hash, file_hash);
                continue; // next file
            }

            // This gets the location of the block of data within the blob file
            let blob_block_location_ref = match blob_index.get_block_location_ref(&chunkmeta.hash) {
                Ok(location) => location,
                Err(e) => {
                    // abort restore of this file, remove TEMP file and move on to next ...
                    eprintln!("Unable to restore file: {:?}", e);
                    continue; // next file
                }
            };
            dbg!(&blob_block_location_ref);

            // Decrypt the blob file and read the necessary data
            let block_data = reader.get_bytes(&blob_block_location_ref).unwrap();
            println!("Read {} bytes from blob file", block_data.len());

            fh.seek(SeekFrom::Start(offset)).await?;
            fh.write_all(&block_data).await?;
            println!(
                "Wrote {} bytes to file {:?}",
                block_data.len(),
                restore_path
            );
            offset += chunkmeta.size as u64;
        }

        // hard links for the same data with multiple filenames
        for other in other_paths.iter() {
            match std::fs::hard_link(restore_path, other) {
                Ok(_) => {
                    println!("Created hard link: {:?}", other);
                }
                Err(e) => {
                    eprintln!("Unable to create hard link {:?}: {:?}", other, e);
                }
            }
        }
    }

    // TODO: iterate the plainindex, decrypt (from blob index ptr) and restore
    // TODO: consider hard links for the same data with multiple filenames

    Ok(())
}
