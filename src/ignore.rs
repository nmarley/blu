//! Filesystem walking with `.bluignore` support.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Filename for blu ignore rules (gitignore syntax).
pub const BLUIGNORE_FILENAME: &str = ".bluignore";

/// Build a walker over `root` that honors `.bluignore` and always skips
/// `.blu/` and `.git/` directories.
pub fn walk_builder(root: impl AsRef<Path>) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(false)
        .hidden(false)
        .parents(true)
        .add_custom_ignore_filename(BLUIGNORE_FILENAME)
        .filter_entry(|entry| {
            let name = entry.file_name();
            name != ".blu" && name != ".git"
        });
    builder
}

/// Normalize a path by stripping a leading `./` when present.
pub fn normalize_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    path.strip_prefix("./")
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Yield regular file paths under `root`, honoring `.bluignore`.
pub fn walk_files(root: impl AsRef<Path>) -> impl Iterator<Item = PathBuf> {
    walk_builder(root).build().filter_map(|entry| {
        let entry = entry.ok()?;
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            return None;
        }
        Some(normalize_path(entry.into_path()))
    })
}

/// Collect regular files under `root` as `(path, size)` pairs.
pub fn walk_files_with_sizes(root: impl AsRef<Path>) -> Vec<(PathBuf, u64)> {
    walk_builder(root)
        .build()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                return None;
            }
            let meta = entry.metadata().ok()?;
            Some((normalize_path(entry.into_path()), meta.len()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::tempdir;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    fn paths_set(root: &Path) -> std::collections::HashSet<PathBuf> {
        walk_files(root)
            .map(|p| p.strip_prefix(root).map(|s| s.to_path_buf()).unwrap_or(p))
            .collect()
    }

    #[test]
    fn bluignore_globs_and_dirs() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write_file(&root.join(".bluignore"), "*.log\nsecret/\n");
        write_file(&root.join("keep.txt"), "keep");
        write_file(&root.join("noise.log"), "noise");
        write_file(&root.join("secret/x.txt"), "secret");
        write_file(&root.join("nested/ok.txt"), "ok");

        let paths = paths_set(root);
        assert!(paths.contains(Path::new("keep.txt")));
        assert!(paths.contains(Path::new("nested/ok.txt")));
        assert!(paths.contains(Path::new(".bluignore")));
        assert!(!paths.contains(Path::new("noise.log")));
        assert!(!paths.contains(Path::new("secret/x.txt")));
    }

    #[test]
    fn always_excludes_blu_and_git_dirs() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write_file(&root.join("visible.txt"), "v");
        write_file(&root.join(".blu/config.toml"), "x");
        write_file(&root.join(".blu/data/blob"), "b");
        write_file(&root.join(".git/config"), "g");
        write_file(&root.join(".git/objects/x"), "o");

        let paths = paths_set(root);
        assert!(paths.contains(Path::new("visible.txt")));
        assert!(!paths.iter().any(|p| p.starts_with(".blu")));
        assert!(!paths.iter().any(|p| p.starts_with(".git")));
    }

    #[test]
    fn nested_bluignore() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write_file(&root.join("top.txt"), "t");
        write_file(&root.join("sub/.bluignore"), "hidden.txt\n");
        write_file(&root.join("sub/visible.txt"), "v");
        write_file(&root.join("sub/hidden.txt"), "h");

        let paths = paths_set(root);
        assert!(paths.contains(Path::new("top.txt")));
        assert!(paths.contains(Path::new("sub/visible.txt")));
        assert!(!paths.contains(Path::new("sub/hidden.txt")));
    }

    #[test]
    fn walk_files_with_sizes_reports_len() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write_file(&root.join("a.txt"), "abcd");

        let files = walk_files_with_sizes(root);
        let rel: Vec<_> = files
            .into_iter()
            .map(|(p, n)| {
                (
                    p.strip_prefix(root).map(|s| s.to_path_buf()).unwrap_or(p),
                    n,
                )
            })
            .collect();
        assert_eq!(rel, vec![(PathBuf::from("a.txt"), 4)]);
    }

    #[test]
    fn plain_index_add_honors_bluignore() {
        use crate::block::PlainIndex;

        let dir = tempdir().unwrap();
        let root = dir.path();
        write_file(&root.join(".bluignore"), "*.log\n");
        write_file(&root.join("keep.txt"), "keep");
        write_file(&root.join("noise.log"), "noise");

        let mut index = PlainIndex::new_empty();
        index.add(root, None).unwrap();

        let paths: std::collections::HashSet<_> = index
            .files
            .values()
            .flat_map(|fr| fr.paths.iter().cloned())
            .map(|p| p.strip_prefix(root).map(|s| s.to_path_buf()).unwrap_or(p))
            .collect();

        assert!(paths.contains(Path::new("keep.txt")));
        assert!(paths.contains(Path::new(".bluignore")));
        assert!(!paths.contains(Path::new("noise.log")));
    }

    #[test]
    fn plain_index_explicit_file_overrides_ignore() {
        use crate::block::PlainIndex;

        let dir = tempdir().unwrap();
        let root = dir.path();
        write_file(&root.join(".bluignore"), "*.log\n");
        write_file(&root.join("noise.log"), "noise");

        let mut index = PlainIndex::new_empty();
        index.add(root.join("noise.log"), None).unwrap();

        let paths: std::collections::HashSet<_> = index
            .files
            .values()
            .flat_map(|fr| fr.paths.iter().cloned())
            .collect();

        assert_eq!(paths.len(), 1);
        assert!(paths.iter().any(|p| p.ends_with("noise.log")));
    }
}
