/// Status check utility, similar to `git status`.
///
/// Two modes: shallow (filesystem metadata only) and deep (hash every file).
/// Deep is the default for vaults under 1 GiB; shallow kicks in above that.
/// Force either with `--type`.
///
/// TODO:
/// - [ ] Display files which are in the PlainIndex but not encrypted
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::block::PlainIndex;
use crate::cli::clapargs::{StatusArgs, StatusCheckType};
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::cli::output::FileDisplay;
use crate::error::BluError;
use crate::format::human_bytes;
use crate::hash::{self, Hash};
use crate::ignore::walk_files_with_sizes;

// 1 GB? (before they ruined the abbreviation)
const SHALLOW_CHECK_BYTE_COUNT: u64 = 1024 * 1024 * 1024;

/// Show the local status of the blu vault
pub fn status(args: StatusArgs) -> Result<(), BluError> {
    let dir = Path::new(".");
    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;
    let index = cfg.load_plain_index(&keys)?;

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
                        let bytes = match std::fs::read(p) {
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
            let curr_fs_index = PlainIndex::new(".")?;

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

    // Display changes (new/deleted/renamed/modified)
    let has_changes = !vec_new_files.is_empty()
        || !vec_removed_files.is_empty()
        || !vec_paths_updated.is_empty()
        || !vec_size_mismatch.is_empty();

    println!();
    if has_changes {
        println!("changes:");
        for fref in &vec_new_files {
            println!("    new:      {}", fref);
        }
        for fref in &vec_removed_files {
            println!("    deleted:  {}", fref);
        }
        for fref in &vec_paths_updated {
            println!("    renamed:  {}", fref);
        }
        for fref in &vec_size_mismatch {
            println!("    modified: {}", fref);
        }
        println!();
    } else {
        println!("no changes detected");
        println!();
    }

    // Vault summary: file and dedup stats from the plain index
    let file_count = index.files_map_ref().len();
    let total_bytes = index.total_bytes_indexed();
    let dedup_bytes = index.duplicate_bytes_indexed();
    let total_chunks = index.count_blocks();

    println!("vault:");
    println!(
        "    files:    {} ({})",
        file_count,
        human_bytes(total_bytes),
    );
    if dedup_bytes > 0 {
        println!("    dedup:    {} saved", human_bytes(dedup_bytes));
    }
    println!("    chunks:   {} unique", total_chunks);

    // Blob / encryption stats
    let blob_index = cfg.load_blob_index_or_default(&keys);

    let blob_file_count = blob_index.count_blob_files();
    let encrypted_chunks = index
        .blocks
        .iter()
        .filter(|(block_hash, _)| blob_index.map.contains_key(block_hash))
        .count();

    if total_chunks == 0 {
        println!("    blobs:    none (no chunks in index)");
    } else {
        let pct = (encrypted_chunks as f64 / total_chunks as f64) * 100.0;
        println!(
            "    blobs:    {} blob files, {} of {} chunks encrypted ({:.1}%)",
            blob_file_count, encrypted_chunks, total_chunks, pct,
        );
    }
    if !blob_index.paths_to_delete.is_empty() {
        println!(
            "    gc:       {} blob files pending deletion",
            blob_index.paths_to_delete.len(),
        );
    }

    // Tag stats
    let tag_index = cfg.load_tag_index_or_default(&keys);

    let unique_tags = tag_index.list_all_tags().len();
    let tagged_files = tag_index.file_tags.len();
    if unique_tags > 0 {
        println!(
            "    tags:     {} unique across {} files",
            unique_tags, tagged_files,
        );
    }

    // Backend listing
    let backend_names: Vec<&str> = cfg.backends.keys().map(|s| s.as_str()).collect();
    let mut backend_parts: Vec<String> = Vec::new();
    for name in &backend_names {
        if *name == cfg.default_backend {
            backend_parts.push(format!("{} (default)", name));
        } else {
            backend_parts.push(name.to_string());
        }
    }
    backend_parts.sort();
    println!("    backends: {}", backend_parts.join(", "));

    println!();
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
    walk_files_with_sizes(dir)
}
