use itertools::Itertools;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::age::BlackBox;
use crate::cli::clapargs::ListFilesArgs;
use crate::config;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// List files in the index, optionally filtered
pub async fn list_files(args: ListFilesArgs) -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(".");
    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);

    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;
    let plain_index = cfg.load_plain_index(&bbox).unwrap();

    let tag_index = cfg.load_tag_index(&bbox).unwrap_or_default();

    // TODO: sort by file name? hash? should the order be deterministic? Since
    // this returns a hash(ref) and we'd have to delve to get filename and make
    // another pass, (also complicated since it's keyed on hash and multiple
    // diverse filename "tags" can exist), let's make sure this is sorted by
    // hash only. Then the bottom sort (below) can display paths in
    // lexicographical order.

    // TODO: maybe add this (sorted file hashes) to index API and add the test there?
    let files_ref = plain_index.files_map_ref();
    let file_hashes = files_ref.keys().sorted_unstable();

    // TODO : consider paths index also?

    // per hash file hash, list the data
    for file_hash in file_hashes {
        let file_ref = files_ref.get(file_hash).unwrap();
        if let Some(ref filter) = args.filter {
            let mut found_match = false;
            // try and filter
            if file_hash.to_string().contains(filter) {
                found_match = true;
                println!("Got a hash match!");
            }

            // TODO: path index?
            if file_ref.paths.iter().any(|p| {
                p.to_string_lossy()
                    .to_lowercase()
                    .contains(&filter.to_lowercase())
            }) {
                found_match = true;
                println!("Got a path match!");
            }

            if tag_index
                .get_tags(file_hash)
                .iter()
                .any(|t| t.contains(&filter.to_lowercase()))
            {
                found_match = true;
                println!("Got a tag match!");
            }

            if !found_match {
                continue;
            };
        };

        let file_size = file_ref.total_size();
        let chunkmetas = &file_ref.chunkmetas;

        println!("  Hash: {}", file_hash.dbg_short(15));
        println!("  Size: {}", file_size);
        // println!("Chunks: {}", chunkmetas.len());

        // Counting chunk size -- not really necessary and probably not good
        // use of resources
        let mut x: HashMap<usize, u32> = HashMap::new();
        for cm in &chunkmetas[0..chunkmetas.len() - 1] {
            // insert or update the hashmap
            x.entry(cm.size).and_modify(|e| *e += 1).or_insert(1);
        }
        let chunk_size_str = match x.len() {
            0 | 1 => format!("{}", chunkmetas[0].size),
            _ => "variable".to_string(),
        };
        // println!("Chunk size: {}", chunk_size_str);
        println!(
            "Chunks: {}, Size: {}, Final chunk size: {}",
            chunkmetas.len(),
            chunk_size_str,
            chunkmetas[chunkmetas.len() - 1].size
        );

        // Here we will display paths in lexicographical order.
        // TODO: probably also need to add this to the API (deterministic
        // sorted paths)
        let mut paths: Vec<&PathBuf> = file_ref.paths.iter().collect();
        paths.sort_unstable();

        let mut tags = tag_index.get_tags(file_hash);
        tags.sort_unstable();
        if !tags.is_empty() {
            println!("  Tags: {}", paths.len());
            for t in tags {
                println!("    - {}", t);
            }
        }

        println!(" Paths: {}", paths.len());
        for p in paths {
            println!("    - {}", p.display());
        }
        println!();
    }

    Ok(())
}

/*
  Hash: "laskjf;lbaksjd;lkjsdf;lkj"
  Size: 2314         FileType: Regular File

 Paths: 3
    - src/bin/list_files.rs
    - src/bin/hahaha
    - dev/joker
*/

/*
Something like this maybe:

stat -x src/bin/list_files.rs
  File: "src/bin/list_files.rs"
  Size: 2314         FileType: Regular File
  Mode: (0644/-rw-r--r--)         Uid: (  502/ nmarley)  Gid: (   20/   staff)
Device: 1,4   Inode: 14796315    Links: 1
Access: Mon Jul  3 16:04:54 2023
Modify: Mon Jul  3 16:04:53 2023
Change: Mon Jul  3 16:04:53 2023
 Birth: Mon Jul  3 16:04:53 2023
*/
