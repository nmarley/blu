#![allow(clippy::uninlined_format_args)]

#[macro_use]
extern crate log;

use clap::Parser;
use simplelog::*;
use std::env;
use std::os::unix::fs::FileExt;
use std::path::Path;

use blu::age::BlackBox;
use blu::blob::EncBlobReader;
use blu::config;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

#[derive(Parser)]
pub struct Args {
    pub dir: String,
    pub restore_paths: Vec<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    CombinedLogger::init(vec![TermLogger::new(
        LevelFilter::Info,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )])
    .unwrap();

    info!("Started restore_files util");

    let args = Args::parse();
    // move into the basedir for all operations, like `git -C <dir>`
    env::set_current_dir(args.dir)?;
    let dir = Path::new(".");

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);

    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;
    let plain_index = cfg.load_plain_index(&bbox).unwrap();
    let blob_index = cfg.load_blob_index(&bbox).unwrap_or_default();

    let backend = cfg.init_storage_backend()?;

    // NOTE:
    //     `*` derefs the `Box<dyn StorageBackend>`
    //     BlobBuffer::new expects a `&dyn StorageBackend`
    let mut reader = EncBlobReader::new(&bbox, &(*backend));

    for (file_hash, file_ref) in plain_index.files_map_ref() {
        println!("========================================================================");
        println!("Restoring file: {:?}", file_hash);

        let file_size = file_ref.total_size();
        println!("Size: {}", file_size);
        println!("Filename(s):");

        // TODO: consider multiple paths... what to do if different paths
        // exist with different filenames?  This might be a UX concern also.

        // check each file path and abort if there is a collision
        for path in file_ref.paths.iter() {
            println!("\t{:?}", path);
            // abort if file exists with this filename
            if std::path::Path::exists(path) {
                eprintln!("Unable to restore file: There already exists in the filesystem a file at the path: {:?}", path);
                continue; // next file
            }
        }

        // TODO: hard links for the same data with multiple filenames

        // TODO: restore to a temp working dir and do a filesystem rename
        // instead of creating the destination file directly

        // after file restored to `restore_path`, create FS hard links to
        // `other_paths`
        let mut path_iter = file_ref.paths.iter();
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
        let fh = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(restore_path)?;
        let _ = fh
            .set_len(file_size)
            .map_err(|e| eprintln!("Unable to set length of new sparse file: {:?}", e));

        let mut offset = 0u64;
        // slowness here ...
        for chunkmeta in file_ref.chunkmetas.iter() {
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
