use std::collections::HashSet;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use glob::Pattern;

use crate::blob::EncBlobReader;
use crate::cli::clapargs::RestoreFilesArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::error::BluError;
use crate::format::human_bytes;
use crate::hash::Hash;

/// Restore plain-text files from the archive, requires index + necessary encrypted blobs
pub fn restore_files(args: RestoreFilesArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Started restore_files util");

    // Validate arguments
    if args.file_hashes.is_empty() && args.path.is_none() && !args.all {
        return Err("Must specify --file-hashes, --path, or --all".into());
    }

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;
    let plain_index = cfg.load_plain_index(&keys)?;
    let blob_index = match cfg.load_blob_index(&keys) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => Default::default(),
        Err(e) => return Err(e.into()),
    };
    let files_map = plain_index.files_map_ref();

    let backend = match &args.backend {
        Some(name) => cfg.init_named_backend(name)?,
        None => cfg.init_storage_backend()?,
    };

    // NOTE:
    //     `*` derefs the `Box<dyn Backend>`
    //     BlobBuffer::new expects a `&dyn Backend`
    let mut reader = EncBlobReader::new(&keys, &(*backend));

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
        println!(
            "Restoring {} ({}, {} chunks)",
            file_hash.dbg_short(9),
            human_bytes(file_size),
            fileref.chunkmetas.len(),
        );

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
            println!("  {}", path.display());
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

        println!("  -> {}", restore_path.display());

        // Create parent directories if needed
        if let Some(parent) = restore_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        // Create a sparse file of the correct size
        let fh = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&restore_path)?;
        let _ = fh
            .set_len(file_size)
            .map_err(|e| eprintln!("Unable to set length of new sparse file: {:?}", e));

        let started = Instant::now();
        let mut offset = 0u64;
        let total_chunks = fileref.chunkmetas.len();

        for (i, chunkmeta) in fileref.chunkmetas.iter().enumerate() {
            if !blob_index.has_chunk(&chunkmeta.hash) {
                // TODO: maybe don't abort (esp. for large files which would
                // piss ppl off), and instead just write the other chunks and
                // log the ones not found in the blob index. The files would be
                // corrupted / not intact so we should report it, but could
                // ostensibly be fixed w/some repair tool if the blobs can be
                // found later.
                eprintln!(
                    "Unable to restore file: Block hash not found in blob index for block: {:?}, file: {:?}",
                    chunkmeta.hash, file_hash
                );
                continue; // next file
            }

            let blob_block_location_ref = match blob_index.get_block_location_ref(&chunkmeta.hash) {
                Ok(location) => location,
                Err(e) => {
                    eprintln!("Unable to restore file: {:?}", e);
                    continue; // next file
                }
            };
            debug!(
                "chunk {}/{}: hash={}, offset={}, size={}",
                i + 1,
                total_chunks,
                chunkmeta.hash.dbg_short(9),
                blob_block_location_ref.position.offset,
                blob_block_location_ref.position.size,
            );

            let block_data = reader.get_bytes(&blob_block_location_ref).unwrap();
            fh.write_all_at(block_data, offset)?;
            trace!(
                "wrote {} bytes at offset {} to {:?}",
                block_data.len(),
                offset,
                restore_path,
            );
            offset += chunkmeta.size as u64;
        }

        let elapsed = started.elapsed();
        let rate = if elapsed.as_secs_f64() > 0.0 {
            file_size as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };
        println!(
            "  restored {} in {:.2}s ({}/s)",
            human_bytes(file_size),
            elapsed.as_secs_f64(),
            human_bytes(rate as u64),
        );

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
