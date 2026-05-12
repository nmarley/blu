//! Backend management subcommands.

use std::path::PathBuf;

use crate::cli::clapargs::{
    BackendAddArgs, BackendArgs, BackendCommand, BackendMirrorArgs, BackendRemoveArgs,
    BackendSetDefaultArgs,
};
use crate::config;
use crate::config::backend::BackendConfig;

/// Dispatch backend subcommands.
pub fn backend(args: BackendArgs) -> Result<(), Box<dyn std::error::Error>> {
    match args.command {
        BackendCommand::Add(a) => add(a),
        BackendCommand::List => list(),
        BackendCommand::Remove(a) => remove(a),
        BackendCommand::SetDefault(a) => set_default(a),
        BackendCommand::Mirror(a) => mirror(a),
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
fn list() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = config::read_config(".")?;

    for (name, backend) in &cfg.backends {
        let is_default = if name == &cfg.default_backend {
            "  (default)"
        } else {
            ""
        };

        let detail = match backend {
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

        println!("{:<16}{}{}", name, detail, is_default);
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

/// Mirror blobs from one backend to another.
fn mirror(args: BackendMirrorArgs) -> Result<(), Box<dyn std::error::Error>> {
    use crate::cli::helpers::{load_config_and_blackbox, LoadOptions};
    use crate::error::BluError;

    let (cfg, bbox) = load_config_and_blackbox(&LoadOptions::default())?;

    if !cfg.backends.contains_key(&args.from) {
        return Err(format!("source backend \"{}\" not found", args.from).into());
    }
    if !cfg.backends.contains_key(&args.to) {
        return Err(format!("destination backend \"{}\" not found", args.to).into());
    }
    if args.from == args.to {
        return Err("source and destination must be different".into());
    }

    let from_backend = cfg.init_named_backend(&args.from)?;
    let to_backend = cfg.init_named_backend(&args.to)?;

    // Load blob index to get all blob paths
    let blob_index = match cfg.load_blob_index(&bbox) {
        Ok(idx) => idx,
        Err(BluError::IndexNotFound(_)) => {
            println!("No blob index found, nothing to mirror");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let blob_paths: Vec<&PathBuf> = blob_index.path_index.keys().collect();
    let total = blob_paths.len();

    if total == 0 {
        println!("No blobs to mirror");
        return Ok(());
    }

    println!(
        "Mirroring {} blob(s) from \"{}\" to \"{}\"",
        total, args.from, args.to
    );

    let mut copied = 0u64;
    let mut skipped = 0u64;
    let mut failed = 0u64;
    let mut bytes_copied = 0u64;

    for (i, path) in blob_paths.iter().enumerate() {
        // Check if destination already has this blob
        match to_backend.exists(path) {
            Ok(true) => {
                skipped += 1;
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                eprintln!(
                    "  [{}/{}] error checking {}: {}",
                    i + 1,
                    total,
                    path.display(),
                    e
                );
                failed += 1;
                continue;
            }
        }

        // Read from source
        let data = match from_backend.read_data(path) {
            Ok(data) => data,
            Err(e) => {
                eprintln!(
                    "  [{}/{}] error reading {}: {}",
                    i + 1,
                    total,
                    path.display(),
                    e
                );
                failed += 1;
                continue;
            }
        };

        // Extract hash from path and write to destination
        let hash = match crate::storage::hash_from_path(path) {
            Ok(h) => h,
            Err(e) => {
                eprintln!(
                    "  [{}/{}] error parsing hash from {}: {}",
                    i + 1,
                    total,
                    path.display(),
                    e
                );
                failed += 1;
                continue;
            }
        };

        match to_backend.write_data(&hash, &data) {
            Ok(_) => {
                bytes_copied += data.len() as u64;
                copied += 1;
            }
            Err(e) => {
                eprintln!(
                    "  [{}/{}] error writing {}: {}",
                    i + 1,
                    total,
                    path.display(),
                    e
                );
                failed += 1;
            }
        }
    }

    println!(
        "Mirror complete: {} copied ({} bytes), {} skipped, {} failed",
        copied,
        crate::format::human_bytes(bytes_copied),
        skipped,
        failed
    );

    if failed > 0 {
        Err(format!("{} blob(s) failed to mirror", failed).into())
    } else {
        Ok(())
    }
}
