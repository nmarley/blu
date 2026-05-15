use std::collections::HashSet;

use crate::cli::clapargs::SearchArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::cli::output::FileDisplay;
use crate::error::BluError;
use crate::hash::Hash;
use crate::search::FilenameSearchIndex;

/// Search for filenames or tags
pub fn search(args: SearchArgs) -> Result<(), BluError> {
    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    // TODO: load search index here ... (once implemented)
    //   for now, just create a new one every time and then search
    let index = cfg.load_plain_index(&keys)?;
    let mut filename_search_index = FilenameSearchIndex::new();
    let files_map = index.files_map_ref();
    for (file_hash, file_ref) in files_map {
        for path in file_ref.paths.iter() {
            let path_str = path.to_string_lossy();
            filename_search_index.add_filename(&path_str, file_hash);
        }
    }

    let mut search_results: HashSet<Hash> = HashSet::new();
    for file_hash in filename_search_index.search(&args.needle) {
        search_results.insert(file_hash.clone());
    }

    // load tag index
    match cfg.load_tag_index(&keys) {
        Ok(tag_index) => {
            for file_hash in tag_index.search(&args.needle) {
                search_results.insert(file_hash.clone());
            }
        }
        Err(BluError::IndexNotFound(_)) => {}
        Err(e) => return Err(e),
    };

    // now print search results
    println!("Got {} result(s):\n", search_results.len());
    for file_hash in search_results {
        let file_ref = match files_map.get(&file_hash) {
            Some(r) => r,
            None => continue,
        };
        let display = FileDisplay {
            hash: file_hash.clone(),
            size: file_ref.total_size(),
            paths: Vec::from_iter(file_ref.paths.iter().cloned()),
        };
        println!("{}", display);
    }

    Ok(())
}
