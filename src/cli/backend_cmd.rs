//! Backend management subcommands.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::cli::clapargs::{
    BackendAddArgs, BackendArgs, BackendCommand, BackendDiffArgs, BackendListArgs,
    BackendMirrorArgs, BackendRemoveArgs, BackendSetDefaultArgs,
};
use crate::config;
use crate::config::backend::BackendConfig;
use crate::error::BluError;

/// Dispatch backend subcommands.
pub async fn backend(args: BackendArgs) -> Result<(), BluError> {
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
fn add(args: BackendAddArgs) -> Result<(), BluError> {
    let mut cfg = config::read_config(".")?;

    if cfg.backends.contains_key(&args.name) {
        return Err(BluError::InvalidConfig(format!(
            "backend \"{}\" already exists",
            args.name
        )));
    }

    let backend_cfg = match args.backend_type.as_str() {
        "local" => {
            let path = args.path.ok_or_else(|| {
                BluError::InvalidConfig("--path is required for local backends".into())
            })?;
            BackendConfig::Local(config::backend::LocalConfig {
                path: PathBuf::from(path),
            })
        }
        "s3" => {
            let bucket = args.bucket.ok_or_else(|| {
                BluError::InvalidConfig("--bucket is required for S3 backends".into())
            })?;
            BackendConfig::AmazonS3(config::backend::S3Config {
                bucket,
                prefix: args.prefix,
                region: args.region,
            })
        }
        other => {
            return Err(BluError::InvalidConfig(format!(
                "unknown backend type: \"{}\"",
                other
            )));
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

/// Count how many of the given paths exist in a backend, concurrently.
async fn count_existing(
    backend: &crate::storage::BackendKind,
    paths: &[PathBuf],
    concurrency: usize,
) -> u64 {
    use std::sync::Arc;

    use indicatif::{ProgressBar, ProgressStyle};
    use tokio::sync::Semaphore;
    use tokio::task::JoinSet;

    let total = paths.len();
    if total == 0 {
        return 0;
    }

    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut tasks = JoinSet::new();

    for path in paths {
        let sem = Arc::clone(&semaphore);
        let be = backend.clone();
        let p = path.clone();

        tasks.spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            matches!(be.exists(&p).await, Ok(true))
        });
    }

    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{bar:40} {pos}/{len} [{elapsed_precise}]")
            .expect("valid progress bar template"),
    );

    let mut present = 0u64;
    while let Some(result) = tasks.join_next().await {
        if matches!(result, Ok(true)) {
            present += 1;
        }
        pb.inc(1);
    }

    pb.finish_and_clear();
    present
}

/// List all configured backends.
async fn list(args: BackendListArgs) -> Result<(), BluError> {
    use crate::cli::helpers::{load_config_and_keys, LoadOptions};

    let cfg = config::read_config(".")?;

    // If --stats, load the blob index to count blobs per backend
    let blob_paths: Option<Vec<PathBuf>> = if args.stats {
        let (cfg2, keys) = load_config_and_keys(&LoadOptions::default())?;
        Some(
            cfg2.load_blob_index_or_default(&keys)
                .path_index
                .keys()
                .cloned()
                .collect(),
        )
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
            let present = count_existing(&be, paths, 16).await;
            format!("  [{}/{} blobs]", present, paths.len())
        } else {
            String::new()
        };

        println!("{:<16}{}{}{}", name, detail, is_default, stats_str);
    }

    Ok(())
}

/// Remove a named backend from the config.
fn remove(args: BackendRemoveArgs) -> Result<(), BluError> {
    let mut cfg = config::read_config(".")?;

    if !cfg.backends.contains_key(&args.name) {
        return Err(BluError::InvalidConfig(format!(
            "backend \"{}\" not found",
            args.name
        )));
    }

    if cfg.default_backend == args.name {
        return Err(BluError::InvalidConfig(format!(
            "cannot remove \"{}\" because it is the default backend; \
             run `blu backend set-default <other>` first",
            args.name
        )));
    }

    cfg.backends.remove(&args.name);
    cfg.save()?;
    println!("Removed backend \"{}\"", args.name);

    Ok(())
}

/// Set the default backend.
fn set_default(args: BackendSetDefaultArgs) -> Result<(), BluError> {
    let mut cfg = config::read_config(".")?;

    if !cfg.backends.contains_key(&args.name) {
        return Err(BluError::InvalidConfig(format!(
            "backend \"{}\" not found",
            args.name
        )));
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
) -> Result<HashSet<PathBuf>, BluError> {
    let tag_index = match cfg.load_tag_index(keys) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => {
            return Err(BluError::IndexNotFound(
                "tag index not found (no tags exist)".into(),
            ));
        }
        Err(e) => return Err(e),
    };

    let file_hashes: Vec<_> = tag_index.search(tag).cloned().collect();
    if file_hashes.is_empty() {
        return Err(BluError::IndexNotFound(format!(
            "no files found with tag \"{}\"",
            tag
        )));
    }

    let plain_index = cfg.load_plain_index(keys)?;
    let blob_index = match cfg.load_blob_index(keys) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => {
            return Err(BluError::IndexNotFound("no blob index found".into()));
        }
        Err(e) => return Err(e),
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

/// Progress event sent from worker tasks to the progress consumer.
enum MirrorEvent {
    /// Destination already had the blob.
    Skipped,
    /// Dry run: blob would have been copied.
    WouldCopy,
    /// Source read complete; carries the number of bytes read.
    ReadComplete(u64),
    /// Destination write complete; carries the number of bytes written.
    WriteComplete(u64),
    /// An error occurred (message included for reporting).
    Failed(String),
}

/// Mirror blobs from one backend to another.
async fn mirror(args: BackendMirrorArgs) -> Result<(), BluError> {
    use std::sync::Arc;

    use indicatif::{ProgressBar, ProgressStyle};
    use tokio::sync::{mpsc, Semaphore};

    use crate::cli::helpers::{load_config_and_keys, LoadOptions};

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    if !cfg.backends.contains_key(&args.from) {
        return Err(BluError::InvalidConfig(format!(
            "source backend \"{}\" not found",
            args.from
        )));
    }
    if !cfg.backends.contains_key(&args.to) {
        return Err(BluError::InvalidConfig(format!(
            "destination backend \"{}\" not found",
            args.to
        )));
    }
    if args.from == args.to {
        return Err(BluError::InvalidConfig(
            "source and destination must be different".into(),
        ));
    }

    let from_backend = cfg.init_named_backend(&args.from).await?;
    let to_backend = cfg.init_named_backend(&args.to).await?;

    // The worker closure below takes ownership of `to_backend`; keep a
    // cheap clone (BackendKind is Arc-backed) to push indexes to the
    // destination after mirroring completes.
    let to_backend_for_push = to_backend.clone();

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
            Err(BluError::IndexLoadFailed { ref reason, .. })
                if reason.contains("deserialization") =>
            {
                println!("Blob index unreadable ({}), nothing to mirror", reason);
                return Ok(());
            }
            Err(e) => return Err(e),
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

    // Channel for progress events from worker tasks
    let (tx, mut rx) = mpsc::channel::<MirrorEvent>(args.jobs as usize * 4);

    // Spawn all worker tasks
    let workers = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();

        for path in blob_paths {
            let sem = Arc::clone(&semaphore);
            let src = from_backend.clone();
            let dst = to_backend.clone();
            let tx = tx.clone();

            tasks.spawn(async move {
                let _permit = sem.acquire().await.expect("semaphore closed");

                // Check if destination already has this blob.
                // Format errors to String for progress channel display.
                let exists = dst
                    .exists(&path)
                    .await
                    .map_err(|e| format!("error checking {}: {}", path.display(), e));

                match exists {
                    Ok(true) => {
                        let _ = tx.send(MirrorEvent::Skipped).await;
                        return;
                    }
                    Ok(false) => {}
                    Err(msg) => {
                        let _ = tx.send(MirrorEvent::Failed(msg)).await;
                        return;
                    }
                }

                if dry_run {
                    let _ = tx.send(MirrorEvent::WouldCopy).await;
                    return;
                }

                // Read from source
                let data = src
                    .read_data(&path)
                    .await
                    .map_err(|e| format!("error reading {}: {}", path.display(), e));

                let data = match data {
                    Ok(data) => data,
                    Err(msg) => {
                        let _ = tx.send(MirrorEvent::Failed(msg)).await;
                        return;
                    }
                };

                let bytes = data.len() as u64;
                let _ = tx.send(MirrorEvent::ReadComplete(bytes)).await;

                // Derive the content hash from the blob data itself
                // rather than the path. The on-disk filename is a raw
                // digest (multihash prefix stripped by path_for), so
                // round-tripping it back through path_for would fail.
                // Re-hashing also verifies data integrity.
                let hash = crate::hash::Hash::from(crate::hash::multihash(&data).to_bytes());

                let result = dst
                    .write_data(&hash, &data)
                    .await
                    .map_err(|e| format!("error writing {}: {}", path.display(), e));

                match result {
                    Ok(_) => {
                        let _ = tx.send(MirrorEvent::WriteComplete(bytes)).await;
                    }
                    Err(msg) => {
                        let _ = tx.send(MirrorEvent::Failed(msg)).await;
                    }
                }
            });
        }

        // Drop our copy of the sender; remaining copies are in tasks.
        // When all tasks finish, all senders drop and rx.recv() returns None.
        drop(tx);
        while tasks.join_next().await.is_some() {}
    });

    // Progress bar consumer: drain channel events and update display
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
    let mut active_reads = 0u64;
    let mut bytes_transferred = 0u64;

    while let Some(event) = rx.recv().await {
        match event {
            MirrorEvent::Skipped => {
                skipped += 1;
                pb.inc(1);
            }
            MirrorEvent::WouldCopy => {
                would_copy += 1;
                pb.inc(1);
            }
            MirrorEvent::ReadComplete(_bytes) => {
                active_reads += 1;
            }
            MirrorEvent::WriteComplete(bytes) => {
                copied += 1;
                active_reads = active_reads.saturating_sub(1);
                bytes_transferred += bytes;
                pb.inc(1);
            }
            MirrorEvent::Failed(msg) => {
                pb.suspend(|| eprintln!("  {}", msg));
                failed += 1;
                pb.inc(1);
            }
        }

        pb.set_message(format!(
            "{} copied, {} skipped, {} uploading, {}",
            copied,
            skipped,
            active_reads,
            crate::format::human_bytes(bytes_transferred),
        ));
    }

    pb.finish_and_clear();

    // Wait for the worker coordinator to finish (should already be done
    // since the channel is drained, but this propagates any panics).
    workers.await?;

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
        return Err(BluError::StorageError(format!(
            "{} blob(s) failed to mirror",
            failed
        )));
    }

    // A mirrored backend is useless without indexes, so push them to the
    // destination after a real (non-dry-run) mirror completes. This makes
    // the destination a complete, recoverable replica.
    if !dry_run {
        println!("Syncing indexes to \"{}\"...", args.to);
        crate::cli::helpers::push_indexes_or_fail(&cfg, Some(&args.to), Some(&to_backend_for_push))
            .await?;
    }

    Ok(())
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
async fn diff(args: BackendDiffArgs) -> Result<(), BluError> {
    use std::sync::Arc;

    use indicatif::{ProgressBar, ProgressStyle};
    use tokio::sync::Semaphore;
    use tokio::task::JoinSet;

    use crate::cli::helpers::{load_config_and_keys, LoadOptions};

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    if !cfg.backends.contains_key(&args.from) {
        return Err(BluError::InvalidConfig(format!(
            "backend \"{}\" not found",
            args.from
        )));
    }
    if !cfg.backends.contains_key(&args.to) {
        return Err(BluError::InvalidConfig(format!(
            "backend \"{}\" not found",
            args.to
        )));
    }

    let from_backend = cfg.init_named_backend(&args.from).await?;
    let to_backend = cfg.init_named_backend(&args.to).await?;

    let blob_index = match cfg.load_blob_index(&keys) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => {
            println!("No blob index found, nothing to diff");
            return Ok(());
        }
        Err(BluError::IndexLoadFailed { ref reason, .. }) if reason.contains("deserialization") => {
            println!("Blob index unreadable ({}), nothing to diff", reason);
            return Ok(());
        }
        Err(e) => return Err(e),
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

            // Check both backends concurrently.
            let (from_res, to_res) = tokio::join!(src.exists(&path), dst.exists(&path),);

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
