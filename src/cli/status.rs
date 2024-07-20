/// This is a basic status check util, similar to `git status` command.
/// Currently it's very rudimentary and checks for new, deleted and renamed
/// files. It has two modes: shallow and deep. Deep mode is the default for blu
/// filesystem dirs smaller than a certain size (1 GB currently), and shallow
/// mode will be invoked above that. The reason for this is that deep mode is a
/// lot slower, as it hashes every file in the blu dir. Shallow mode only checks
/// filesystem metadata, and then hashes files that don't match what is expected
/// from the index.
///
/// It is possible to force shallow or deep mode with the `--type` option.
///
/// Probably has bugs.
///
/// TODO:
/// - [ ] Display files which are in the PlainIndex but not encrypted
/// - [ ] Display stats, e.g. # files, # bytes de-duplicated (saved), x tags
///       being used, etc.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::fs;
use walkdir::WalkDir;

use crate::age::BlackBox;
use crate::block::PlainIndex;
use crate::cli::clapargs::{StatusArgs, StatusCheckType};
use crate::cli::output::FileDisplay;
use crate::config;
use crate::hash::{self, Hash};
use crate::ignore;

// 1 GB? (before they ruined the abbreviation)
const SHALLOW_CHECK_BYTE_COUNT: u64 = 1024 * 1024 * 1024;
const TEST_AGE_SECRET_KEY: &str = include_str!("../../test/blu_secrets/blu.key");

/// Show the local status of the blu vault
pub async fn status(args: StatusArgs) -> Result<(), Box<dyn std::error::Error>> {
    // info!("Started status util");
    let dir = Path::new(".");

    let cfg = config::read_config(dir).await.map_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
        e
    })?;

    let bbox = BlackBox::new(&[TEST_AGE_SECRET_KEY]);
    let index = match cfg.load_plain_index(&bbox) {
        Some(idx) => idx,
        None => return Err("unable to load index".into()),
    };
    // dbg!(&index);

    // TODO:
    // show files existing in FS but not in index ...
    //   note: Does this mean scanning entire dir every time? Or do we do a short version where we
    //   don't read EVERY file but instead match up expected paths and then only show new ones? And
    //   then could have a --deep or some kind of arg to deep scan entire dir and hash every file?
    // show existing in index but not in FS ... (or moved?)

    // Walk files in fs dir (all regular files -- Walkdir on the main bludir)
    // check in paths index for each filename. If a filename exists that's not in the index,
    // report. But what about renamed files? Or files w/swapped names? Do we really not want to
    // report them?
    //
    // Note: this is really only an issue due to regular path-based filesystems, in a content-based
    // system this wouldn't be an issue b/c each entry (each new file) would have a content hash.
    // Would be an attribute accessible on the file as well, even if paths must be used to
    // reference them (it shouldn't be required).
    // TODO?

    let files_list = get_files_and_sizes(dir);
    dbg!(&files_list);
    // select deep or shallow check type based on cmd-line arg (explicit) or blu
    // dir size (implicit / variable)
    let current_dir_size = files_list.iter().fold(0, |acc, elem| acc + elem.1);
    let status_check_type = match args.status_check_type {
        Some(t) => t,
        None => {
            if current_dir_size >= SHALLOW_CHECK_BYTE_COUNT {
                StatusCheckType::Shallow
            } else {
                StatusCheckType::Deep
            }
        }
    };

    let mut vec_new_files: Vec<FileDisplay> = Vec::new();
    let mut vec_removed_files: Vec<FileDisplay> = Vec::new();
    let mut vec_size_mismatch: Vec<FileSizeMismatch> = Vec::new();
    let mut vec_paths_updated: HashSet<FileUpdatedPaths> = HashSet::new();

    match status_check_type {
        StatusCheckType::Shallow => {
            let mut curr_fs_path_size: HashMap<PathBuf, u64> = HashMap::new();
            let path_index = index.build_path_index();
            // files that ARE NOT in the index but ARE in FS (new files)
            'outer: for (p, s) in files_list.iter() {
                curr_fs_path_size.insert(p.clone(), *s);
                match path_index.get(p) {
                    None => {
                        // TODO: consider full-file hash here too, and comparing
                        // w/index
                        //
                        // calculates hash here
                        let bytes = match fs::read(p).await {
                            Ok(b) => b,
                            Err(e) => {
                                println!("unable to read file {:?}: {}", p, e);
                                continue 'outer;
                            }
                        };
                        let mh = hash::multihash(&bytes);
                        let hash = Hash::from(mh.to_bytes());
                        vec_new_files.push(FileDisplay {
                            hash,
                            size: *s,
                            paths: vec![p.clone()],
                        });
                    }
                    Some(val) => {
                        let fileref = match index.get_fileref_ref(val) {
                            Some(f) => f,
                            None => {
                                println!("error: unable to get fileref for {:?}", p);
                                continue 'outer;
                            }
                        };
                        let size_in_index = fileref.total_size();
                        if size_in_index != *s {
                            // TODO: consider full-file hash on only these files
                            // which have sizes which don't match the same
                            // filename in the index
                            //
                            // NOTE: If there is a size mismatch, then
                            // technically it's a different file, so consider
                            // hashing the file and comparing the hash to the
                            // index. If the hash matches something, then it's a
                            // rename and we should report it as such.
                            vec_size_mismatch.push(FileSizeMismatch {
                                hash: val.clone(),
                                size_in_index,
                                size_in_filesystem: *s,
                                paths: fileref.paths.iter().cloned().collect(),
                            });
                        }
                    }
                };
            }

            // files that WERE in the index but got removed (deleted from fs)
            for (p, file_hash) in path_index.iter() {
                if !curr_fs_path_size.contains_key(p) {
                    let fileref = match index.get_fileref_ref(file_hash) {
                        Some(f) => f,
                        None => {
                            println!("error: unable to get fileref for {:?}", p);
                            continue;
                        }
                    };
                    vec_removed_files.push(FileDisplay {
                        hash: file_hash.clone(),
                        size: fileref.total_size(),
                        paths: fileref.paths.iter().cloned().collect(),
                    });
                }
            }
        }
        StatusCheckType::Deep => {
            // index the current dir on filesystem and compare
            let curr_fs_index = PlainIndex::new(".").await?;

            // files that ARE NOT in the index but ARE in FS
            for (file_hash, fileref) in &curr_fs_index.files {
                match index.files.get(file_hash) {
                    Some(val) => {
                        // Same same, now check paths
                        if val.paths != fileref.paths {
                            vec_paths_updated.insert(FileUpdatedPaths {
                                hash: file_hash.clone(),
                                size: fileref.total_size(),
                                paths_in_index: val.paths.iter().cloned().collect(),
                                paths_in_filesystem: fileref.paths.iter().cloned().collect(),
                            });
                        }
                    }
                    None => {
                        // File does NOT exist in index, therefore it's new
                        vec_new_files.push(FileDisplay {
                            hash: file_hash.clone(),
                            size: fileref.total_size(),
                            paths: fileref.paths.iter().cloned().collect(),
                        });
                    }
                };
            }

            // files that WERE in the index but got removed ...
            for (file_hash, fileref) in &index.files {
                match curr_fs_index.files.get(file_hash) {
                    Some(val) => {
                        // Same same, now check paths
                        if val.paths != fileref.paths {
                            vec_paths_updated.insert(FileUpdatedPaths {
                                hash: file_hash.clone(),
                                size: fileref.total_size(),
                                paths_in_index: fileref.paths.iter().cloned().collect(),
                                paths_in_filesystem: val.paths.iter().cloned().collect(),
                            });
                        }
                    }
                    None => {
                        // File does NOT exist in FS, therefore it has been
                        // removed.
                        vec_removed_files.push(FileDisplay {
                            hash: file_hash.clone(),
                            size: fileref.total_size(),
                            paths: fileref.paths.iter().cloned().collect(),
                        });
                    }
                }
            }
        }
    };

    // now display results
    println!();
    if !vec_new_files.is_empty() {
        println!("new: ");
        for fref in vec_new_files {
            println!("    {}", fref);
        }
        println!();
    }

    if !vec_removed_files.is_empty() {
        println!("deleted: ");
        for fref in vec_removed_files {
            println!("    {}", fref);
        }
        println!();
    }

    if !vec_paths_updated.is_empty() {
        println!("path(s) updated (renamed on filesystem): ");
        for fref in vec_paths_updated {
            println!("    {}", fref);
        }
        println!();
    }

    if !vec_size_mismatch.is_empty() {
        println!("size mismatch: ");
        for fref in vec_size_mismatch {
            println!("    {}", fref);
        }
        println!();
    }

    // Now show encrypted status ... but the thing is, _files_ are not encrypted, but rather the chunks
    // are. So we need to iterate over the chunks and see if they are encrypted or not.
    let blob_index = match cfg.load_blob_index(&bbox) {
        Some(idx) => idx,
        None => {
            println!("no blob index found, assuming no files are encrypted");
            return Ok(());
        }
    };

    let count_encrypted_chunks = index
        .blocks
        .iter()
        .filter(|(block_hash, _blockref)| blob_index.map.contains_key(block_hash))
        .count();
    let total_chunks = index.blocks.len();
    let encrypted_pct = (count_encrypted_chunks as f64 / total_chunks as f64) * 100.0;

    println!(
        "{} of {} chunks in index are encrypted ({:.2}%)",
        count_encrypted_chunks, total_chunks, encrypted_pct
    );

    Ok(())
}

#[derive(Clone, Debug)]
pub struct FileSizeMismatch {
    pub hash: Hash,
    pub size_in_index: u64,
    pub size_in_filesystem: u64,
    pub paths: Vec<PathBuf>,
}

impl std::fmt::Display for FileSizeMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let display_hash = self.hash.dbg_short(7);
        write!(
            f,
            "hash: {}, index_size: {}, fs_size: {}, paths: {:?}",
            display_hash, self.size_in_index, self.size_in_filesystem, self.paths,
        )
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct FileUpdatedPaths {
    pub hash: Hash,
    pub size: u64,
    pub paths_in_index: Vec<PathBuf>,
    pub paths_in_filesystem: Vec<PathBuf>,
}

impl std::fmt::Display for FileUpdatedPaths {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let display_hash = self.hash.dbg_short(7);
        write!(
            f,
            "hash: {}, size: {}, index_paths: {:?}, fs_paths: {:?}",
            display_hash, self.size, self.paths_in_index, self.paths_in_filesystem,
        )
    }
}

fn get_files_and_sizes<P: AsRef<Path>>(dir: P) -> Vec<(PathBuf, u64)> {
    let ignore_patterns = ignore::get_bluignore_patterns();
    dbg!(&ignore_patterns);

    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| {
            // here, e = Result<walkdir::DirEntry>
            let e = match e.ok() {
                Some(elem) => elem,
                None => return None,
            };

            // remove non-files
            if !e.path().is_file() {
                return None;
            }

            // get file size from metadata
            let md = match e.metadata().ok() {
                Some(elem) => elem,
                None => return None,
            };
            let file_size = md.len();

            // strip leading ./
            let path = e.path().to_path_buf();
            let path = path
                .strip_prefix("./")
                .unwrap_or_else(|_| &path)
                .to_path_buf();

            // ignore files in .blu dir
            if path.starts_with(".blu/") {
                return None;
            }

            // ignore patterns from .bluignore file
            for pattern in ignore_patterns.iter() {
                dbg!(&pattern, &path);
                if path.starts_with(pattern) {
                    println!(
                        "Got a match! path: [{:?}], pattern: [{:?}]",
                        &path, &pattern
                    );
                    return None;
                } else {
                    println!(
                        "path: [{:?}] does not start with pattern: [{:?}]",
                        &path, &pattern
                    );
                }
            }
            println!("hi");
            // if ignore_patterns
            //     .iter()
            //     .any(|pattern| path.starts_with(pattern))
            // {
            //     return None;
            // }

            Some((path, file_size))
        })
        .collect::<Vec<(PathBuf, u64)>>()
}
