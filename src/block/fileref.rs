use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::ChunkMeta;
use crate::error::BluError;

/// Filesystem identity for one path that holds this content.
///
/// Used as a local short-circuit so backup can skip full content hashing
/// when mtime and size still match. Not a substitute for the content hash.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Eq, Default)]
pub struct PathStat {
    /// Modified time as Unix UTC seconds (`mtime`).
    pub mtime_secs: i64,
    /// On-disk size in bytes (`st_size`).
    pub size: u64,
}

impl PathStat {
    /// Create a PathStat from mtime seconds and size.
    pub fn new(mtime_secs: i64, size: u64) -> Self {
        Self { mtime_secs, size }
    }
}

/// Build a path map with default (zero) PathStats for each path.
///
/// Useful in tests and fixtures that only care about path membership.
pub fn path_stats_from<I, P>(paths: I) -> HashMap<PathBuf, PathStat>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    paths
        .into_iter()
        .map(|p| (p.as_ref().to_path_buf(), PathStat::default()))
        .collect()
}

/// FileRef is a container encapsulating a Vec<ChunkMeta> (collection of hashes
/// of chunks read from a fs::File) and filesystem references to it (filenames)
#[derive(PartialEq, Clone, Serialize, Deserialize, Eq)]
pub struct FileRef {
    /// Ordered list of chunk hashes and sizes that make up this file
    pub chunkmetas: Vec<ChunkMeta>,
    /// Filesystem paths that share this exact content, with local PathStat
    pub paths: HashMap<PathBuf, PathStat>,
}

impl std::fmt::Debug for FileRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "FileRef {{ chunkmetas.len(): {}, paths: {:?} }}",
            self.chunkmetas.len(),
            self.paths
        )
    }
}

impl PartialOrd for FileRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FileRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.chunkmetas.cmp(&other.chunkmetas)
    }
}

impl FileRef {
    /// Create a new FileRef with the given chunkmetas and an empty
    /// paths map.
    pub fn new(f: Vec<ChunkMeta>) -> Self {
        Self {
            chunkmetas: f,
            paths: HashMap::new(),
        }
    }

    /// Build a FileRef with paths recorded under default (zero) PathStats.
    ///
    /// Intended for tests and callers that do not yet have real mtime/size.
    pub fn with_paths<I, P>(chunkmetas: Vec<ChunkMeta>, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let mut fr = Self::new(chunkmetas);
        for p in paths {
            fr.paths.insert(p.into(), PathStat::default());
        }
        fr
    }

    /// Record or refresh PathStat for a path on this content.
    pub fn insert_path<P: Into<PathBuf>>(&mut self, path: P, stat: PathStat) {
        self.paths.insert(path.into(), stat);
    }

    /// Whether this FileRef claims `path`.
    pub fn contains_path<P: AsRef<Path>>(&self, path: P) -> bool {
        self.paths.contains_key(path.as_ref())
    }

    /// Return any one path from the paths map, or an error if empty.
    pub fn get_a_path(&self) -> Result<PathBuf, BluError> {
        self.paths
            .keys()
            .next()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| BluError::IndexCorrupted("fileref has no paths".into()))
    }

    /// Compute the total file size in bytes by summing all chunk sizes.
    pub fn total_size(&self) -> u64 {
        self.chunkmetas
            .iter()
            .fold(0, |acc, elem| acc + elem.size as u64)
    }

    /// Merge remote path stats into this FileRef.
    ///
    /// Same path keeps the PathStat with the greater `mtime_secs` (tie keeps
    /// local). New paths are inserted.
    pub fn merge_paths_from(&mut self, other: &FileRef) {
        for (path, remote_stat) in &other.paths {
            self.paths
                .entry(path.clone())
                .and_modify(|local_stat| {
                    if remote_stat.mtime_secs > local_stat.mtime_secs {
                        *local_stat = remote_stat.clone();
                    }
                })
                .or_insert_with(|| remote_stat.clone());
        }
    }
}

/// Compare FileRef maps by content hash, chunkmetas, and path keys only.
///
/// PathStat values are ignored so fixtures need not freeze mtimes.
#[cfg(test)]
pub(crate) fn assert_files_match_ignoring_pathstat(
    left: &HashMap<crate::hash::Hash, FileRef>,
    right: &HashMap<crate::hash::Hash, FileRef>,
) {
    assert_eq!(left.len(), right.len(), "file map length");
    for (hash, lfr) in left {
        let rfr = right
            .get(hash)
            .unwrap_or_else(|| panic!("missing hash {hash}"));
        assert_eq!(lfr.chunkmetas, rfr.chunkmetas, "chunkmetas for {hash}");
        let lp: std::collections::HashSet<&PathBuf> = lfr.paths.keys().collect();
        let rp: std::collections::HashSet<&PathBuf> = rfr.paths.keys().collect();
        assert_eq!(lp, rp, "path keys for {hash}");
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::hash::{self, Hash};

    fn dummy_chunk() -> ChunkMeta {
        let data = b"x";
        ChunkMeta {
            hash: Hash::from(hash::multihash(data).to_bytes()),
            size: data.len(),
        }
    }

    #[test]
    fn pathstat_round_trip_cbor() {
        let fr = FileRef::with_paths(
            vec![dummy_chunk()],
            [PathBuf::from("a/b.txt"), PathBuf::from("c/d.txt")],
        );
        let mut fr2 = fr.clone();
        fr2.insert_path("a/b.txt", PathStat::new(1_700_000_000, 42));

        let mut bytes = Vec::new();
        ciborium::into_writer(&fr2, &mut bytes).unwrap();
        let back: FileRef = ciborium::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(back.paths.len(), 2);
        assert_eq!(
            back.paths.get(Path::new("a/b.txt")),
            Some(&PathStat::new(1_700_000_000, 42))
        );
        assert!(back.contains_path("c/d.txt"));
    }

    #[test]
    fn merge_paths_prefers_newer_mtime() {
        let mut local = FileRef::new(vec![dummy_chunk()]);
        local.insert_path("same.txt", PathStat::new(100, 10));
        local.insert_path("only-local.txt", PathStat::new(50, 1));

        let mut remote = FileRef::new(vec![dummy_chunk()]);
        remote.insert_path("same.txt", PathStat::new(200, 10));
        remote.insert_path("only-remote.txt", PathStat::new(60, 2));

        local.merge_paths_from(&remote);
        assert_eq!(
            local.paths.get(Path::new("same.txt")),
            Some(&PathStat::new(200, 10))
        );
        assert!(local.contains_path("only-local.txt"));
        assert!(local.contains_path("only-remote.txt"));
    }
}
