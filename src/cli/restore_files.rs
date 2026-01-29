use std::collections::HashSet;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use glob::Pattern;

use crate::blob::EncBlobReader;
use crate::cli::clapargs::RestoreFilesArgs;
use crate::cli::helpers::{load_config_and_blackbox, LoadOptions};
use crate::hash::Hash;

/// Restore plain-text files from the archive, requires index + necessary encrypted blobs
pub fn restore_files(args: RestoreFilesArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Started restore_files util");

    // Validate arguments
    if args.file_hashes.is_empty() && args.path.is_none() && !args.all {
        return Err("Must specify --file-hashes, --path, or --all".into());
    }

    let (cfg, bbox) = load_config_and_blackbox(&LoadOptions::default())?;
    let plain_index = cfg.load_plain_index(&bbox).unwrap();
    let blob_index = cfg.load_blob_index(&bbox).unwrap_or_default();
    let files_map = plain_index.files_map_ref();

    let backend = cfg.init_storage_backend()?;

    // NOTE:
    //     `*` derefs the `Box<dyn StorageBackend>`
    //     BlobBuffer::new expects a `&dyn StorageBackend`
    let mut reader = EncBlobReader::new(&bbox, &(*backend));

    // Build path pattern matcher if specified
    let path_pattern = args.path.as_ref().map(|p| {
        Pattern::new(p).unwrap_or_else(|e| {
            warn!("Invalid glob pattern '{}': {}, treating as literal", p, e);
            Pattern::new(&glob::Pattern::escape(p)).unwrap()
        })
    });

    // Collect files to restore
    let mut unique_hashes: HashSet<Hash> = HashSet::new();

    for (hash, fileref) in files_map.iter() {
        let mut should_restore = false;

        // Check if --all
        if args.all {
            should_restore = true;
        }

        // Check if hash matches any provided hash prefix
        if !args.file_hashes.is_empty() {
            let hash_str = hash.to_string();
            for h in &args.file_hashes {
                if hash_str.contains(h) {
                    println!("Got a match on file hash: {}", hash.dbg_short(9));
                    should_restore = true;
                    break;
                }
            }
        }

        // Check if any path matches the pattern
        if let Some(ref pattern) = path_pattern {
            for path in &fileref.paths {
                if pattern.matches_path(path) {
                    println!("Got a match on path: {}", path.display());
                    should_restore = true;
                    break;
                }
            }
        }

        if should_restore {
            unique_hashes.insert(hash.clone());
        }
    }

    if unique_hashes.is_empty() {
        println!("No files matched the specified criteria");
        return Ok(());
    }

    println!("Found {} file(s) to restore", unique_hashes.len());

    // Parse destination directory
    let dest_dir = args.to.as_ref().map(PathBuf::from);

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

        // Determine restore path(s) based on --to option
        let (restore_path, other_paths): (PathBuf, Vec<PathBuf>) = if let Some(ref dest) = dest_dir
        {
            // Restore to destination directory with original filename
            let first_path = fileref.paths.iter().next().unwrap();
            let filename = first_path.file_name().unwrap();
            let dest_path = Path::new(dest).join(filename);

            // For --to mode, we only restore to one location (no hard links)
            (dest_path, vec![])
        } else {
            // Restore to original paths
            let mut path_iter = fileref.paths.iter();
            let first = path_iter.next().unwrap().clone();
            let others = path_iter.cloned().collect::<Vec<_>>();
            (first, others)
        };

        // Print all original paths
        for path in fileref.paths.iter() {
            println!("\t{:?}", path);
        }

        // Check if destination file exists
        if restore_path.exists() {
            eprintln!(
                "Unable to restore file: There already exists a file at: {:?}",
                restore_path
            );
            continue 'outer;
        }

        // Check other paths too (only in non --to mode)
        for other in &other_paths {
            if other.exists() {
                eprintln!(
                    "Unable to restore file: There already exists a file at: {:?}",
                    other
                );
                continue 'outer;
            }
        }

        println!("Restoring to: '{}'", restore_path.display());

        // Create parent directories if needed
        if let Some(parent) = restore_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        // Create a sparse file of the correct size
        println!("Creating sparse file of size: {}", file_size);
        let fh = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(&restore_path)?;
        let _ = fh
            .set_len(file_size)
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

            fh.write_all_at(&block_data, offset)?;
            println!(
                "Wrote {} bytes to file {:?}",
                block_data.len(),
                restore_path
            );
            offset += chunkmeta.size as u64;
        }

        // hard links for the same data with multiple filenames
        for other in &other_paths {
            // Create parent directories if needed
            if let Some(parent) = other.parent() {
                if !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!(
                            "Unable to create parent dir for hard link {:?}: {:?}",
                            other, e
                        );
                        continue;
                    }
                }
            }
            match std::fs::hard_link(&restore_path, other) {
                Ok(_) => {
                    println!("Created hard link: {:?}", other);
                }
                Err(e) => {
                    eprintln!("Unable to create hard link {:?}: {:?}", other, e);
                }
            }
        }
    }

    Ok(())
}
