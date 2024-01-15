use std::collections::HashSet;
use std::path::Path;

use crate::age::BlackBox;
use crate::cli::clapargs::SearchArgs;
use crate::config;
use crate::hash::Hash;
use crate::search::FilenameSearchIndex;

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// Search for filenames or tags
pub fn search(args: SearchArgs) -> Result<(), Box<dyn std::error::Error>> {
    // info!("Started search util");

    let dir = Path::new(".");

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let cfg = config::read_config(dir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;

    // TODO: load search index here ... (once implemented)
    //   for now, just create a new one every time and then search
    let index = cfg.load_plain_index(&bbox).unwrap();
    let mut filename_search_index = FilenameSearchIndex::new();
    let files_map = index.files_map_ref();
    for (file_hash, file_ref) in files_map {
        // info!("file_hash: {}", file_hash.dbg_short(7));
        for path in file_ref.paths.iter() {
            filename_search_index.add_filename(path.to_str().unwrap(), file_hash);
        }
    }

    let search_result = filename_search_index.search(&args.needle);
    info!("Got {} result(s)", search_result.len());
    for file_hash in search_result.iter() {
        info!("{:?}", file_hash);
        // let file_ref = files_map.get(file_hash).unwrap();
        // info!("{:?}", file_ref);
    }

    // load tag index
    if let Some(tag_index) = cfg.load_tag_index(&bbox) {
        let tag_search_result: HashSet<&Hash> = tag_index.search(&args.needle).collect();
        dbg!(&tag_search_result);
    };

    // TODO: unify the results above and use a FileDisplay to display the
    // output, kinda status does

    Ok(())
}
