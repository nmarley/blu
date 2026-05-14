//! Backend management subcommands.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::cli::clapargs::{
    BackendAddArgs, BackendArgs, BackendCommand, BackendDiffArgs, BackendListArgs,
    BackendMirrorArgs, BackendRemoveArgs, BackendSetDefaultArgs,
};
use crate::config;
use crate::config::backend::BackendConfig;

/// Dispatch backend subcommands.
pub async fn backend(args: BackendArgs) -> Result<(), Box<dyn std::error::Error>> {
    match args.command {
        BackendCommand::Add(a) => add(a),
        BackendCommand::List(a) => list(a).await,
        BackendCommand::Remove(a) => remove(a),
        BackendCommand::SetDefault(a) => set_default(a),
        BackendCommand::Mirror(a) => mirror(a).await,
        BackendCommand::Diff(a) => diff(a).await,
    }
}

/// Add a named storage backend to the config.
fn add(args: BackendAddArgs) -> Result<(), Box<dyn std::error::Error>> {
    let mut cfg = config::read_config(".")?;

    if cfg.backends.contains_key(&args.name) {
        return Err(format!("backend \"{}\" already exists", args.name).into());
    }

    let backend_cfg = match args.backend_type.as_str() {
        "local" => {
            let path = args.path.ok_or("--path is required for local backends")?;
            BackendConfig::Local(config::backend::LocalConfig {
                path: PathBuf::from(path),
            })
        }
        "s3" => {
            let bucket = args.bucket.ok_or("--bucket is required for S3 backends")?;
            BackendConfig::AmazonS3(config::backend::S3Config {
                bucket,
                prefix: args.prefix,
                region: args.region,
            })
        }
        other => {
            return Err(format!("unknown backend type: \"{}\"", other).into());
        }
    };

    cfg.backends.insert(args.name.clone(), backend_cfg);

    if args.default {
        cfg.default_backend = args.name.clone();
    }

    cfg.save()?;
    println!("Added backend \"{}\"", args.name);

    if args.default {
        println!("Set \"{}\" as default", args.name);
    }

    Ok(())
}

/// List all configured backends.
async fn list(args: BackendListArgs) -> Result<(), Box<dyn std::error::Error>> {
    use crate::cli::helpers::{load_config_and_keys, LoadOptions};
    use crate::error::BluError;

    let cfg = config::read_config(".")?;

    // If --stats, load the blob index to count blobs per backend
    let blob_paths: Option<HashSet<PathBuf>> = if args.stats {
        let (cfg2, keys) = load_config_and_keys(&LoadOptions::default())?;
        match cfg2.load_blob_index(&keys) {
            Ok(idx) => Some(idx.path_index.keys().cloned().collect()),
            Err(BluError::IndexNotFound(_)) => Some(HashSet::new()),
            Err(e) => return Err(e.into()),
        }
    } else {
        None
    };

    for (name, backend_cfg) in &cfg.backends {
        let is_default = if name == &cfg.default_backend {
            "  (default)"
        } else {
            ""
        };

        let detail = match backend_cfg {
            BackendConfig::Local(local) => {
                format!("local  path={}", local.path.display())
            }
            BackendConfig::AmazonS3(s3) => {
                let mut parts = vec![format!("bucket={}", s3.bucket)];
                if let Some(ref prefix) = s3.prefix {
                    parts.push(format!("prefix={}", prefix));
                }
                if let Some(ref region) = s3.region {
                    parts.push(format!("region={}", region));
                }
                format!("s3     {}", parts.join(" "))
            }
        };

        let stats_str = if let Some(ref paths) = blob_paths {
            let be = cfg.init_named_backend(name).await?;
            let mut present = 0u64;
            for path in paths {
                match be.exists(path).await {
                    Ok(true) => present += 1,
                    Ok(false) => {}
                    Err(_) => {}
                }
            }
            format!("  [{}/{} blobs]", present, paths.len())
        } else {
            String::new()
        };

        println!("{:<16}{}{}{}", name, detail, is_default, stats_str);
    }

    Ok(())
}

/// Remove a named backend from the config.
fn remove(args: BackendRemoveArgs) -> Result<(), Box<dyn std::error::Error>> {
    let mut cfg = config::read_config(".")?;

    if !cfg.backends.contains_key(&args.name) {
        return Err(format!("backend \"{}\" not found", args.name).into());
    }

    if cfg.default_backend == args.name {
        return Err(format!(
            "cannot remove \"{}\" because it is the default backend; \
             run `blu backend set-default <other>` first",
            args.name
        )
        .into());
    }

    cfg.backends.remove(&args.name);
    cfg.save()?;
    println!("Removed backend \"{}\"", args.name);

    Ok(())
}

/// Set the default backend.
fn set_default(args: BackendSetDefaultArgs) -> Result<(), Box<dyn std::error::Error>> {
    let mut cfg = config::read_config(".")?;

    if !cfg.backends.contains_key(&args.name) {
        return Err(format!("backend \"{}\" not found", args.name).into());
    }

    cfg.default_backend = args.name.clone();
    cfg.save()?;
    println!("Default backend set to \"{}\"", args.name);

    Ok(())
}

/// Collect the set of blob paths relevant to a given tag.
///
/// Joins tag index -> plain index (file hashes -> chunk hashes) ->
/// blob index (chunk hashes -> blob paths).
fn blob_paths_for_tag(
    tag: &str,
    cfg: &config::Config,
    keys: &crate::dek_provider::DekProvider,
) -> Result<HashSet<PathBuf>, Box<dyn std::error::Error>> {
    use crate::error::BluError;

    let tag_index = match cfg.load_tag_index(keys) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => {
            return Err("tag index not found (no tags exist)".into());
        }
        Err(e) => return Err(e.into()),
    };

    let file_hashes: Vec<_> = tag_index.search(tag).cloned().collect();
    if file_hashes.is_empty() {
        return Err(format!("no files found with tag \"{}\"", tag).into());
    }

    let plain_index = cfg.load_plain_index(keys)?;
    let blob_index = match cfg.load_blob_index(keys) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => {
            return Err("no blob index found".into());
        }
        Err(e) => return Err(e.into()),
    };

    let mut paths = HashSet::new();
    for file_hash in &file_hashes {
        if let Some(file_ref) = plain_index.get_fileref_ref(file_hash) {
            for cm in &file_ref.chunkmetas {
                if let Ok(loc) = blob_index.get_block_location_ref(&cm.hash) {
                    paths.insert(loc.blob_path().clone());
                }
            }
        }
    }

    Ok(paths)
}

/// Outcome of a single blob mirror task.
enum MirrorResult {
    /// Blob was copied; carries the number of bytes written.
    Copied(u64),
    /// Destination already had the blob.
    Skipped,
    /// Dry run: blob would have been copied.
    WouldCopy,
    /// An error occurred (message included for reporting).
    Failed(String),
}

/// Mirror blobs from one backend to another.
async fn mirror(args: BackendMirrorArgs) -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;

    use indicatif::{ProgressBar, ProgressStyle};
    use tokio::sync::Semaphore;
    use tokio::task::JoinSet;

    use crate::cli::helpers::{load_config_and_keys, LoadOptions};
    use crate::error::BluError;

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    if !cfg.backends.contains_key(&args.from) {
        return Err(format!("source backend \"{}\" not found", args.from).into());
    }
    if !cfg.backends.contains_key(&args.to) {
        return Err(format!("destination backend \"{}\" not found", args.to).into());
    }
    if args.from == args.to {
        return Err("source and destination must be different".into());
    }

    let from_backend = cfg.init_named_backend(&args.from).await?;
    let to_backend = cfg.init_named_backend(&args.to).await?;

    // Determine which blob paths to mirror
    let blob_paths_set: HashSet<PathBuf> = if let Some(ref tag) = args.tag {
        blob_paths_for_tag(tag, &cfg, &keys)?
    } else {
        let blob_index = match cfg.load_blob_index(&keys) {
            Ok(idx) => idx,
            Err(BluError::IndexNotFound(_)) => {
                println!("No blob index found, nothing to mirror");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        blob_index.path_index.keys().cloned().collect()
    };

    let blob_paths: Vec<PathBuf> = blob_paths_set.into_iter().collect();
    let total = blob_paths.len();

    if total == 0 {
        println!("No blobs to mirror");
        return Ok(());
    }

    let mode = if args.dry_run { " (dry run)" } else { "" };
    let tag_info = if let Some(ref tag) = args.tag {
        format!(" [tag: {}]", tag)
    } else {
        String::new()
    };

    println!(
        "Mirroring {} blob(s) from \"{}\" to \"{}\"{}{}",
        total, args.from, args.to, tag_info, mode
    );

    let semaphore = Arc::new(Semaphore::new(args.jobs as usize));
    let dry_run = args.dry_run;
    let mut tasks = JoinSet::new();

    for path in blob_paths {
        let sem = Arc::clone(&semaphore);
        let src = from_backend.clone();
        let dst = to_backend.clone();

        tasks.spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");

            // Check if destination already has this blob
            match dst.exists(&path).await {
                Ok(true) => return MirrorResult::Skipped,
                Ok(false) => {}
                Err(e) => {
                    return MirrorResult::Failed(format!(
                        "error checking {}: {}",
                        path.display(),
                        e
                    ));
                }
            }

            if dry_run {
                return MirrorResult::WouldCopy;
            }

            // Read from source
            let data = match src.read_data(&path).await {
                Ok(data) => data,
                Err(e) => {
                    return MirrorResult::Failed(format!(
                        "error reading {}: {}",
                        path.display(),
                        e
                    ));
                }
            };

            // Derive the content hash from the blob data itself rather
            // than the path. The on-disk filename is a raw digest
            // (multihash prefix stripped by path_for), so round-tripping
            // it back through path_for would fail. Re-hashing also
            // verifies data integrity.
            let hash = crate::hash::Hash::from(crate::hash::multihash(&data).to_bytes());

            match dst.write_data(&hash, &data).await {
                Ok(_) => MirrorResult::Copied(data.len() as u64),
                Err(e) => MirrorResult::Failed(format!("error writing {}: {}", path.display(), e)),
            }
        });
    }

    // Progress bar, updated as tasks complete
    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{bar:40} {pos}/{len} [{elapsed_precise}] {msg}")
            .expect("valid progress bar template"),
    );

    let mut copied = 0u64;
    let mut skipped = 0u64;
    let mut would_copy = 0u64;
    let mut failed = 0u64;
    let mut bytes_transferred = 0u64;

    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(MirrorResult::Copied(n)) => {
                copied += 1;
                bytes_transferred += n;
            }
            Ok(MirrorResult::Skipped) => skipped += 1,
            Ok(MirrorResult::WouldCopy) => would_copy += 1,
            Ok(MirrorResult::Failed(msg)) => {
                pb.suspend(|| eprintln!("  {}", msg));
                failed += 1;
            }
            Err(e) => {
                pb.suspend(|| eprintln!("  task panicked: {}", e));
                failed += 1;
            }
        }
        pb.inc(1);
    }

    pb.finish_and_clear();

    if dry_run {
        println!(
            "Dry run complete: {} would be copied, {} already present",
            would_copy, skipped
        );
    } else {
        println!(
            "Mirror complete: {} copied ({}), {} skipped, {} failed",
            copied,
            crate::format::human_bytes(bytes_transferred),
            skipped,
            failed
        );
    }

    if failed > 0 {
        Err(format!("{} blob(s) failed to mirror", failed).into())
    } else {
        Ok(())
    }
}

/// Outcome of a single blob diff task.
enum DiffResult {
    /// Blob exists in both backends.
    Both,
    /// Blob exists only in the source ("from") backend.
    FromOnly,
    /// Blob exists only in the destination ("to") backend.
    ToOnly,
    /// Blob not found in either backend (includes path for warning).
    Neither(String),
    /// An error occurred checking existence (message for reporting).
    Error(String),
}

/// Compare blob sets between two backends.
async fn diff(args: BackendDiffArgs) -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;

    use indicatif::{ProgressBar, ProgressStyle};
    use tokio::sync::Semaphore;
    use tokio::task::JoinSet;

    use crate::cli::helpers::{load_config_and_keys, LoadOptions};
    use crate::error::BluError;

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    if !cfg.backends.contains_key(&args.from) {
        return Err(format!("backend \"{}\" not found", args.from).into());
    }
    if !cfg.backends.contains_key(&args.to) {
        return Err(format!("backend \"{}\" not found", args.to).into());
    }

    let from_backend = cfg.init_named_backend(&args.from).await?;
    let to_backend = cfg.init_named_backend(&args.to).await?;

    let blob_index = match cfg.load_blob_index(&keys) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => {
            println!("No blob index found, nothing to diff");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let blob_paths: Vec<PathBuf> = blob_index.path_index.into_keys().collect();
    let total = blob_paths.len();

    if total == 0 {
        println!("No blobs in index");
        return Ok(());
    }

    println!(
        "Comparing {} blob(s) between \"{}\" and \"{}\"",
        total, args.from, args.to
    );

    let from_name = args.from.clone();
    let to_name = args.to.clone();
    let semaphore = Arc::new(Semaphore::new(args.jobs as usize));
    let mut tasks = JoinSet::new();

    for path in blob_paths {
        let sem = Arc::clone(&semaphore);
        let src = from_backend.clone();
        let dst = to_backend.clone();
        let f_name = from_name.clone();
        let t_name = to_name.clone();

        tasks.spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");

            // Check both backends concurrently. Map errors to
            // String so the future is Send (Box<dyn Error> is not).
            let (from_res, to_res) = tokio::join!(
                async { src.exists(&path).await.map_err(|e| e.to_string()) },
                async { dst.exists(&path).await.map_err(|e| e.to_string()) },
            );

            let in_from = match from_res {
                Ok(v) => v,
                Err(e) => {
                    return DiffResult::Error(format!(
                        "error checking {} in \"{}\": {}",
                        path.display(),
                        f_name,
                        e
                    ));
                }
            };
            let in_to = match to_res {
                Ok(v) => v,
                Err(e) => {
                    return DiffResult::Error(format!(
                        "error checking {} in \"{}\": {}",
                        path.display(),
                        t_name,
                        e
                    ));
                }
            };

            match (in_from, in_to) {
                (true, true) => DiffResult::Both,
                (true, false) => DiffResult::FromOnly,
                (false, true) => DiffResult::ToOnly,
                (false, false) => DiffResult::Neither(path.display().to_string()),
            }
        });
    }

    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{bar:40} {pos}/{len} [{elapsed_precise}]")
            .expect("valid progress bar template"),
    );

    let mut both = 0u64;
    let mut from_only = 0u64;
    let mut to_only = 0u64;
    let mut errors = 0u64;

    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(DiffResult::Both) => both += 1,
            Ok(DiffResult::FromOnly) => from_only += 1,
            Ok(DiffResult::ToOnly) => to_only += 1,
            Ok(DiffResult::Neither(p)) => {
                pb.suspend(|| {
                    eprintln!("  warning: {} not found in either backend", p);
                });
            }
            Ok(DiffResult::Error(msg)) => {
                pb.suspend(|| eprintln!("  {}", msg));
                errors += 1;
            }
            Err(e) => {
                pb.suspend(|| eprintln!("  task panicked: {}", e));
                errors += 1;
            }
        }
        pb.inc(1);
    }

    pb.finish_and_clear();

    println!();
    println!("  both:           {}", both);
    println!("  \"{}\" only:  {}", from_name, from_only);
    println!("  \"{}\" only:  {}", to_name, to_only);
    if errors > 0 {
        println!("  errors:         {}", errors);
    }

    Ok(())
}
