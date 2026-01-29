use std::collections::HashSet;

use crate::cli::clapargs::SearchArgs;
use crate::cli::helpers::{load_config_and_blackbox, LoadOptions};
use crate::cli::output::FileDisplay;
use crate::hash::Hash;
use crate::search::FilenameSearchIndex;

/// Search for filenames or tags
pub fn search(args: SearchArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (cfg, bbox) = load_config_and_blackbox(&LoadOptions::default())?;

    // TODO: load search index here ... (once implemented)
    //   for now, just create a new one every time and then search
    let index = cfg.load_plain_index(&bbox).unwrap();
    let mut filename_search_index = FilenameSearchIndex::new();
    let files_map = index.files_map_ref();
    for (file_hash, file_ref) in files_map {
        for path in file_ref.paths.iter() {
            filename_search_index.add_filename(path.to_str().unwrap(), file_hash);
        }
    }

    let mut search_results: HashSet<Hash> = HashSet::new();
    for file_hash in filename_search_index.search(&args.needle) {
        search_results.insert(file_hash.clone());
    }

    // load tag index
    if let Some(tag_index) = cfg.load_tag_index(&bbox) {
        for file_hash in tag_index.search(&args.needle) {
            search_results.insert(file_hash.clone());
        }
    };

    // now print search results
    println!("Got {} result(s):\n", search_results.len());
    for file_hash in search_results {
        let file_ref = files_map.get(&file_hash).unwrap();
        let display = FileDisplay {
            hash: file_hash.clone(),
            size: file_ref.total_size(),
            paths: Vec::from_iter(file_ref.paths.iter().cloned()),
        };
        println!("{}", display);
    }

    Ok(())
}
