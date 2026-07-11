//! Union-merge of plain and blob indexes for multi-device sync.
//!
//! Content-addressed file and chunk maps are merged by hash. Same content
//! hash on both sides keeps both path sets. Path conflicts (one path mapped
//! to two content hashes) are reported but both FileRefs are retained.
//! Blob locations prefer the local entry when a chunk hash already exists.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::blob::BlobIndex;
use crate::block::BlockRef;
use crate::block::{FileRef, PlainIndex, CURRENT_INDEX_VERSION};
use crate::error::BluError;
use crate::hash::Hash;

/// A vault path that maps to more than one content hash after a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathConflict {
    /// Path present under multiple content hashes.
    pub path: PathBuf,
    /// Distinct content hashes that claim this path.
    pub hashes: Vec<Hash>,
}

/// Result of merging two plain indexes.
#[derive(Debug, Clone)]
pub struct PlainIndexMerge {
    /// Union of files and blocks from both sides.
    pub index: PlainIndex,
    /// Paths claimed by more than one content hash.
    pub conflicts: Vec<PathConflict>,
}

/// Merge two plain indexes by content-hash union.
///
/// - Files: union by content hash; path sets are unioned for matching hashes.
/// - Blocks: union by block hash; file-hash references are unioned.
/// - Schema versions must match.
/// - `created_at` is the earlier of the two; `updated_at` is the later.
pub fn merge_plain_index(
    local: &PlainIndex,
    remote: &PlainIndex,
) -> Result<PlainIndexMerge, BluError> {
    if local.version != remote.version {
        return Err(BluError::IndexCorrupted(format!(
            "plain index version mismatch: local={}, remote={} (expected {})",
            local.version, remote.version, CURRENT_INDEX_VERSION
        )));
    }

    let mut files: HashMap<Hash, FileRef> = local.files.clone();
    for (hash, remote_fr) in &remote.files {
        match files.get_mut(hash) {
            Some(local_fr) => {
                if local_fr.chunkmetas != remote_fr.chunkmetas {
                    return Err(BluError::IndexCorrupted(format!(
                        "file hash {} has conflicting chunkmetas across indexes",
                        hash.dbg_short(12)
                    )));
                }
                local_fr.paths.extend(remote_fr.paths.iter().cloned());
            }
            None => {
                files.insert(hash.clone(), remote_fr.clone());
            }
        }
    }

    let mut blocks: HashMap<Hash, BlockRef> = local.blocks.clone();
    for (hash, remote_br) in &remote.blocks {
        match blocks.get_mut(hash) {
            Some(local_br) => {
                for (file_hash, pos) in &remote_br.references {
                    local_br
                        .references
                        .entry(file_hash.clone())
                        .or_insert_with(|| pos.clone());
                }
            }
            None => {
                blocks.insert(hash.clone(), remote_br.clone());
            }
        }
    }

    let created_at = local.created_at.min(remote.created_at);
    let updated_at = local.updated_at.max(remote.updated_at);

    let index = PlainIndex {
        files,
        blocks,
        version: local.version.clone(),
        created_at,
        updated_at,
    };
    let conflicts = detect_path_conflicts(&index);

    Ok(PlainIndexMerge { index, conflicts })
}

/// Merge two blob indexes by chunk-hash union.
///
/// When both sides list the same chunk hash, the local location is kept.
/// `paths_to_delete` / `paths_to_repack` are unioned, then scrubbed so a
/// path that still has live chunks is not marked for deletion.
pub fn merge_blob_index(local: &BlobIndex, remote: &BlobIndex) -> BlobIndex {
    let mut out = local.clone();

    for (chunk_hash, location) in &remote.map {
        if !out.has_chunk(chunk_hash) {
            out.add_chunk_location(chunk_hash, location);
        }
    }

    out.paths_to_delete
        .extend(remote.paths_to_delete.iter().cloned());
    out.paths_to_repack
        .extend(remote.paths_to_repack.iter().cloned());

    out.paths_to_delete
        .retain(|p| !out.path_index.contains_key(p));
    out.paths_to_repack
        .retain(|p| out.path_index.contains_key(p) && !out.paths_to_delete.contains(p));

    out
}

fn detect_path_conflicts(index: &PlainIndex) -> Vec<PathConflict> {
    let mut path_to_hashes: HashMap<PathBuf, HashSet<Hash>> = HashMap::new();
    for (file_hash, fileref) in index.files_map_ref() {
        for path in &fileref.paths {
            path_to_hashes
                .entry(path.clone())
                .or_default()
                .insert(file_hash.clone());
        }
    }

    let mut conflicts: Vec<PathConflict> = path_to_hashes
        .into_iter()
        .filter(|(_, hashes)| hashes.len() > 1)
        .map(|(path, hashes)| {
            let mut hashes: Vec<Hash> = hashes.into_iter().collect();
            hashes.sort();
            PathConflict { path, hashes }
        })
        .collect();
    conflicts.sort_by(|a, b| a.path.cmp(&b.path));
    conflicts
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::blob::BlobBlockLocation;
    use crate::block::ChunkMeta;
    use crate::hash;
    use crate::io::Position;
    use chrono::DateTime;
    use std::path::Path;

    fn h(label: &str) -> Hash {
        Hash::from(hash::multihash(label.as_bytes()).to_bytes())
    }

    fn ts(secs: i64) -> chrono::NaiveDateTime {
        DateTime::from_timestamp(secs, 0).unwrap().naive_utc()
    }

    fn file_ref(chunk_label: &str, paths: &[&str]) -> (Hash, FileRef) {
        let chunk_hash = h(chunk_label);
        let file_hash = h(&format!("file:{chunk_label}"));
        let mut fr = FileRef::new(vec![ChunkMeta {
            hash: chunk_hash.clone(),
            size: 16,
        }]);
        for p in paths {
            fr.paths.insert(PathBuf::from(p));
        }
        (file_hash, fr)
    }

    fn block_ref(file_hash: &Hash) -> BlockRef {
        let mut br = BlockRef::new();
        br.references.insert(
            file_hash.clone(),
            Position {
                offset: 0,
                size: 16,
            },
        );
        br
    }

    fn plain_with(files: Vec<(Hash, FileRef)>, created: i64, updated: i64) -> PlainIndex {
        let mut index = PlainIndex::new_empty();
        index.created_at = ts(created);
        index.updated_at = ts(updated);
        for (file_hash, fr) in files {
            for cm in &fr.chunkmetas {
                let br = block_ref(&file_hash);
                index
                    .blocks
                    .entry(cm.hash.clone())
                    .and_modify(|existing| {
                        existing
                            .references
                            .insert(file_hash.clone(), br.references[&file_hash].clone());
                    })
                    .or_insert(br);
            }
            index.files.insert(file_hash, fr);
        }
        index
    }

    fn loc(path: &str, offset: usize) -> BlobBlockLocation {
        BlobBlockLocation::new(PathBuf::from(path), Position { offset, size: 16 })
    }

    #[test]
    fn merge_empty_local_takes_remote() {
        let remote = plain_with(vec![file_ref("a", &["a.txt"])], 100, 200);
        let local = PlainIndex::new_empty();
        let merged = merge_plain_index(&local, &remote).unwrap();
        assert_eq!(merged.index.files_map_ref().len(), 1);
        assert!(merged.conflicts.is_empty());
        assert!(basenames(&merged.index).contains("a.txt"));
    }

    #[test]
    fn merge_empty_remote_keeps_local() {
        let local = plain_with(vec![file_ref("a", &["a.txt"])], 100, 200);
        let remote = PlainIndex::new_empty();
        let merged = merge_plain_index(&local, &remote).unwrap();
        assert_eq!(merged.index.files_map_ref().len(), 1);
        assert!(basenames(&merged.index).contains("a.txt"));
    }

    #[test]
    fn merge_disjoint_adds_union() {
        let local = plain_with(vec![file_ref("a", &["a_only.txt"])], 100, 150);
        let remote = plain_with(vec![file_ref("b", &["b_only.txt"])], 110, 200);
        let merged = merge_plain_index(&local, &remote).unwrap();
        assert_eq!(merged.index.files_map_ref().len(), 2);
        assert_eq!(merged.index.blocks_map_ref().len(), 2);
        let names = basenames(&merged.index);
        assert!(names.contains("a_only.txt"));
        assert!(names.contains("b_only.txt"));
        assert!(merged.conflicts.is_empty());
        assert_eq!(merged.index.created_at, ts(100));
        assert_eq!(merged.index.updated_at(), ts(200));
    }

    #[test]
    fn merge_same_hash_unions_paths() {
        let (hash, fr_local) = file_ref("same", &["path/a.txt"]);
        let chunk_hash = fr_local.chunkmetas[0].hash.clone();
        let mut fr_remote = FileRef::new(fr_local.chunkmetas.clone());
        fr_remote.paths.insert(PathBuf::from("path/b.txt"));

        let mut local = PlainIndex::new_empty();
        local.files.insert(hash.clone(), fr_local);
        local.blocks.insert(chunk_hash.clone(), block_ref(&hash));

        let mut remote = PlainIndex::new_empty();
        remote.files.insert(hash.clone(), fr_remote);
        remote.blocks.insert(chunk_hash, block_ref(&hash));

        let merged = merge_plain_index(&local, &remote).unwrap();
        assert_eq!(merged.index.files_map_ref().len(), 1);
        let paths = &merged.index.files_map_ref()[&hash].paths;
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(Path::new("path/a.txt")));
        assert!(paths.contains(Path::new("path/b.txt")));
        assert!(merged.conflicts.is_empty());
    }

    #[test]
    fn merge_path_conflict_reported_both_kept() {
        let local = plain_with(vec![file_ref("v1", &["shared.txt"])], 1, 2);
        let remote = plain_with(vec![file_ref("v2", &["shared.txt"])], 1, 3);
        let merged = merge_plain_index(&local, &remote).unwrap();
        assert_eq!(merged.index.files_map_ref().len(), 2);
        assert_eq!(merged.conflicts.len(), 1);
        assert_eq!(merged.conflicts[0].path, PathBuf::from("shared.txt"));
        assert_eq!(merged.conflicts[0].hashes.len(), 2);
    }

    #[test]
    fn merge_block_references_union() {
        let chunk = h("chunk");
        let file_a = h("file-a");
        let file_b = h("file-b");

        let mut local = PlainIndex::new_empty();
        let mut fr_a = FileRef::new(vec![ChunkMeta {
            hash: chunk.clone(),
            size: 16,
        }]);
        fr_a.paths.insert(PathBuf::from("a.txt"));
        local.files.insert(file_a.clone(), fr_a);
        local.blocks.insert(chunk.clone(), block_ref(&file_a));

        let mut remote = PlainIndex::new_empty();
        let mut fr_b = FileRef::new(vec![ChunkMeta {
            hash: chunk.clone(),
            size: 16,
        }]);
        fr_b.paths.insert(PathBuf::from("b.txt"));
        remote.files.insert(file_b.clone(), fr_b);
        remote.blocks.insert(chunk.clone(), block_ref(&file_b));

        let merged = merge_plain_index(&local, &remote).unwrap();
        let br = merged.index.blocks_map_ref().get(&chunk).unwrap();
        assert!(br.references.contains_key(&file_a));
        assert!(br.references.contains_key(&file_b));
    }

    #[test]
    fn merge_version_mismatch_errors() {
        let mut local = PlainIndex::new_empty();
        local.version = "0.0.0".into();
        let remote = PlainIndex::new_empty();
        let err = merge_plain_index(&local, &remote).unwrap_err();
        assert!(err.to_string().contains("version mismatch"));
    }

    #[test]
    fn merge_conflicting_chunkmetas_errors() {
        let file_hash = h("file");
        let mut local = PlainIndex::new_empty();
        local.files.insert(
            file_hash.clone(),
            FileRef::new(vec![ChunkMeta {
                hash: h("c1"),
                size: 16,
            }]),
        );
        let mut remote = PlainIndex::new_empty();
        remote.files.insert(
            file_hash,
            FileRef::new(vec![ChunkMeta {
                hash: h("c2"),
                size: 16,
            }]),
        );
        let err = merge_plain_index(&local, &remote).unwrap_err();
        assert!(err.to_string().contains("conflicting chunkmetas"));
    }

    #[test]
    fn merge_blob_disjoint_union() {
        let mut local = BlobIndex::new();
        local.add_chunk_location(&h("c1"), &loc("blob-a", 0));
        let mut remote = BlobIndex::new();
        remote.add_chunk_location(&h("c2"), &loc("blob-b", 0));

        let merged = merge_blob_index(&local, &remote);
        assert!(merged.has_chunk(&h("c1")));
        assert!(merged.has_chunk(&h("c2")));
        assert_eq!(merged.path_index.len(), 2);
    }

    #[test]
    fn merge_blob_prefers_local_location() {
        let chunk = h("shared-chunk");
        let mut local = BlobIndex::new();
        local.add_chunk_location(&chunk, &loc("local-blob", 0));
        let mut remote = BlobIndex::new();
        remote.add_chunk_location(&chunk, &loc("remote-blob", 32));

        let merged = merge_blob_index(&local, &remote);
        let loc = merged.get_block_location_ref(&chunk).unwrap();
        assert_eq!(loc.blob_path(), &PathBuf::from("local-blob"));
        assert_eq!(loc.position.offset, 0);
    }

    #[test]
    fn merge_blob_scrubs_delete_for_live_path() {
        let mut local = BlobIndex::new();
        local.add_chunk_location(&h("live"), &loc("still-live", 0));
        local.paths_to_delete.insert(PathBuf::from("still-live"));
        local.paths_to_delete.insert(PathBuf::from("already-gone"));

        let remote = BlobIndex::new();
        let merged = merge_blob_index(&local, &remote);
        assert!(!merged.paths_to_delete.contains(Path::new("still-live")));
        assert!(merged.paths_to_delete.contains(Path::new("already-gone")));
    }

    fn basenames(index: &PlainIndex) -> HashSet<String> {
        index
            .files_map_ref()
            .values()
            .flat_map(|fr| fr.paths.iter())
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect()
    }
}
