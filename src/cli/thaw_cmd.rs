//! `blu thaw`: initiate and report S3 archive restores for vault blobs.

use std::time::Duration;

use crate::cli::clapargs::ThawArgs;
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::error::BluError;
use crate::storage::RestoreTier;
use crate::thaw::{
    self, all_indexed_blob_paths, classify_blobs, default_restore_options, format_cold_summary,
    initiate_thaw, plan_blob_set, wait_until_readable, Selection,
};

const CLASSIFY_CONCURRENCY: usize = 16;
const THAW_CONCURRENCY: usize = 8;
const DEFAULT_POLL_SECS: u64 = 30;

/// Initiate or report archive restores for blobs needed by a catalog selection.
pub async fn thaw(args: ThawArgs) -> Result<(), BluError> {
    info!("Started thaw");

    let selection = Selection {
        all: args.all,
        hash_prefixes: args.file_hashes.clone(),
        path_glob: args.path.clone(),
    };

    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;
    let plain_index = cfg.load_plain_index(&keys)?;
    let blob_index = cfg.load_blob_index_or_default(&keys);

    let backend = match &args.backend {
        Some(name) => cfg.init_named_backend(name).await?,
        None => cfg.init_storage_backend().await?,
    };

    let blob_paths = if selection.is_empty() {
        if !args.status {
            return Err(BluError::Internal(
                "Must specify --file-hashes, --path, --all, or --status \
                 (status alone classifies every indexed blob)"
                    .into(),
            ));
        }
        all_indexed_blob_paths(&blob_index)
    } else {
        let set = plan_blob_set(&plain_index, &blob_index, &selection)?;
        if set.file_hashes.is_empty() {
            println!("No files matched the specified criteria");
            return Ok(());
        }
        println!(
            "Selected {} file(s) -> {} unique blob(s)",
            set.file_hashes.len(),
            set.blob_paths.len(),
        );
        set.blob_paths
    };

    if blob_paths.is_empty() {
        println!("No blobs to classify");
        return Ok(());
    }

    let mut plan = classify_blobs(&backend, &blob_paths, CLASSIFY_CONCURRENCY).await?;
    print_plan(&plan);

    if args.status {
        return finalize_status(&plan);
    }

    if plan.blocked_count() == 0 && plan.errors.is_empty() && plan.missing.is_empty() {
        println!("All blobs are readable; nothing to thaw");
        return Ok(());
    }

    let mut opts = default_restore_options();
    if args.standard {
        opts.tier = RestoreTier::Standard;
    }
    if let Some(days) = args.days {
        opts.days = days;
    }

    if !plan.archived.is_empty() {
        println!(
            "Initiating restore for {} archived blob(s) (tier={:?}, days={})...",
            plan.archived.len(),
            opts.tier,
            opts.days,
        );
        let init = initiate_thaw(&backend, &plan, &opts, THAW_CONCURRENCY).await?;
        println!(
            "  initiated={} already_restoring={} failed={}",
            init.initiated.len(),
            init.already_restoring.len(),
            init.failed.len(),
        );
        for (path, err) in &init.failed {
            eprintln!("  failed {}: {}", path.display(), err);
        }
        if !init.failed.is_empty() {
            return Err(BluError::StorageError(format!(
                "{} blob restore request(s) failed",
                init.failed.len()
            )));
        }
        // Refresh after initiate so status reflects Restoring.
        plan = classify_blobs(&backend, &blob_paths, CLASSIFY_CONCURRENCY).await?;
        print_plan(&plan);
    }

    if args.wait {
        if plan.blocked_count() == 0 {
            println!("All blobs readable");
            return finalize_status(&plan);
        }
        println!(
            "Waiting for {} blob(s) to become readable (poll every {}s)...",
            plan.blocked_count(),
            DEFAULT_POLL_SECS,
        );
        let timeout = args.timeout_hours.map(|h| Duration::from_secs(h * 3600));
        plan = wait_until_readable(
            &backend,
            &blob_paths,
            CLASSIFY_CONCURRENCY,
            Duration::from_secs(DEFAULT_POLL_SECS),
            timeout,
        )
        .await?;
        print_plan(&plan);
        println!("All requested blobs are readable");
    } else if plan.blocked_count() > 0 {
        println!(
            "Still blocked: {} blob(s). Retry later, or re-run with --wait.",
            plan.blocked_count(),
        );
    }

    finalize_status(&plan)
}

fn print_plan(plan: &thaw::ColdPlan) {
    println!("Cold status: {}", format_cold_summary(plan));
    for cold in &plan.archived {
        println!(
            "  archived  {} class={:?} archive={:?}",
            cold.path.display(),
            cold.stat.storage_class,
            cold.stat.archive_status,
        );
    }
    for cold in &plan.restoring {
        println!(
            "  restoring {} restore={:?}",
            cold.path.display(),
            cold.stat.restore_header,
        );
    }
    for path in &plan.missing {
        println!("  missing   {}", path.display());
    }
    for (path, err) in &plan.errors {
        println!("  error     {}: {}", path.display(), err);
    }
}

fn finalize_status(plan: &thaw::ColdPlan) -> Result<(), BluError> {
    if !plan.errors.is_empty() {
        return Err(BluError::StorageError(format!(
            "{} blob stat error(s)",
            plan.errors.len()
        )));
    }
    if !plan.missing.is_empty() {
        return Err(BluError::StorageError(format!(
            "{} blob(s) missing from backend",
            plan.missing.len()
        )));
    }
    Ok(())
}
