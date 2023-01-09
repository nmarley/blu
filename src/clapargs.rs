use clap::Parser;

/// Blu - de-duplicated filing system w/encrypted cloud backup
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// The subcommand to run
    #[command(subcommand)]
    pub action: Action,
}

/// The possible subcommands to be run from blu-cli
#[derive(Debug, clap::Subcommand, Clone)]
pub enum Action {
    /// Initialize
    Init,
    /// Add files
    Add,
    /// Restore files from the index
    Restore,
    /// List all tags in the tag index
    ListTags,
    #[command(hide = true)]
    /// Print (debug) the index. Deprecated.
    PrintIndex,
}
