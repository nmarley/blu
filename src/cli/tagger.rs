use std::collections::HashSet;
use std::path::Path;
use walkdir::WalkDir;

use crate::age::BlackBox;
use crate::cli::clapargs::TaggerArgs;
use crate::config;
use crate::hash::{multihash, Hash};
use crate::tag::sanitize_tag;

// Here we implement a tagspec, which are that tags with a leading colon char `:` prefix will be
// removed, like pushing git branches
//
// e.g.: `--tags hello,world,:foo` will add tags `hello`, and `world`, but
// delete tag `foo`
//
// This is a simpler alternative to --add and --remove actions/subcommands.

const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// Manipulate tags on data
pub fn tagger(args: TaggerArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Started tagger util");

    if args.dry_run {
        info!("Got dry_run flag -- will not write tag index");
    }

    info!("Got args: {:?}", &args);

    let basedir = Path::new(".");

    info!("Got blu BASEDIR: {}", basedir.display());
    // determine DEST Path relative to BASEDIR

    // TODO: for each dest given ...
    // for path in args.dest {
    let mut rel_path = Path::new(&args.dest).strip_prefix(basedir)?;
    // }

    if Path::new("").eq(rel_path) {
        rel_path = Path::new(".");
    }
    info!("Got relative path: {}", rel_path.display(),);

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let cfg = config::read_config(basedir).map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;
    // dbg!(&cfg);

    // note that this is a "content tagger" -- I'd much rather keep the hashing
    // in the index only and tag based on what is shown in the index rather
    // than what is on the filesystem, introducing multiple points of
    // interaction w/the filesystem (bad).

    let index = cfg.load_plain_index(&bbox).unwrap();
    let mut tag_index = cfg.load_tag_index(&bbox).unwrap_or_default();
    let files_map = index.files_map_ref();

    let tag_action = &args.tag_action;
    let tags = if let Some(tags) = tag_action.tags.as_ref() {
        tags.split(',').map(sanitize_tag).collect()
    } else {
        vec![]
    };

    let mut unique_hashes: HashSet<Hash> = HashSet::new();
    if let Some(data_hash) = args.data_hash_filter {
        // println!("data_hash(filter): {}", data_hash);
        for hash in files_map.keys() {
            // println!("hash.to_string(): {}", hash);
            if hash.to_string().contains(&data_hash) {
                println!(
                    "Got a hash match on data hash filter: {}",
                    hash.dbg_short(9)
                );
                unique_hashes.insert(hash.clone());
            }
        }
    } else {
        // now WALK all files/dirs within rel_path -- that could be either a
        // file or a dir these will be the targets to find (match) from the
        // index and perform tag operations on
        for elem in WalkDir::new(rel_path).into_iter().filter_map(|e| e.ok()) {
            // ignore internal .blu data + config
            if elem.path().starts_with("./.blu") {
                continue;
            }
            // skip non-files (dirs)
            if !elem.file_type().is_file() {
                continue;
            }

            // TODO: now suck 'em up and match on the index -- files hash only, no chunks
            /*
            let mut hasher = Sha2_512::default();
            let chunker = Chunkerator::new(elem.path(), CHUNK_SIZE)?;
            for chunk in chunker {
                hasher.update(&chunk);
            }
            let file_mh = Code::Sha2_512.wrap(hasher.finalize())?;
            let file_hash = Hash::from(file_mh.to_bytes());
            */
            let file_data = std::fs::read(elem.path())?;
            let file_mh = multihash(&file_data);
            let file_hash = Hash::from(file_mh.to_bytes());
            println!("file_hash = {}", file_hash.dbg_short(7));
            unique_hashes.insert(file_hash);
        }
    }

    for hash in unique_hashes.iter() {
        if !files_map.contains_key(hash) {
            continue;
        };

        println!("hash: {} found in plain index", hash.dbg_short(9));

        if tag_action.remove_all_tags {
            println!("removing all tags");
            tag_index.drop_all_tags(hash);
            continue;
        }

        for tag in tags.iter() {
            if let Some(tag) = tag.strip_prefix(':') {
                println!("removing tag: {}", tag);
                tag_index.remove_tag(hash, tag);
            } else {
                println!("adding tag: {}", tag);
                tag_index.add_tag(hash, tag);
            }
        }
    }

    if args.dry_run {
        println!("DRY-RUN: Refusing to write tag index");
    } else {
        match cfg.write_tag_index(&tag_index, &bbox) {
            Ok(_) => println!("Wrote tag index!"),
            Err(e) => println!("Error writing tag index: {}", e),
        }
    }

    Ok(())
}
