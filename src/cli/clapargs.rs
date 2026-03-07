use clap::Parser;

/// blu - de-duplicated file archival system w/encrypted cloud backup
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// The target folder for blu to run/operate in, like `git -C`
    #[arg(long, default_value = ".")]
    pub bludir: String,

    /// Do not prompt for passphrase (fail if key is encrypted)
    #[arg(long, global = true)]
    pub no_passphrase: bool,

    /// The subcommand to run
    #[command(subcommand)]
    pub action: Action,
}

/// The possible subcommands to be run from blu-cli
#[derive(Debug, clap::Subcommand, Clone)]
pub enum Action {
    /// Add files to the index
    Add(AddArgs),
    /// Initialize a new blu vault
    Init(InitArgs),
    /// Sync files: add to index and encrypt (combines add + encrypt-files)
    Sync(SyncArgs),
    /// Pull indexes from remote backend
    Pull(PullArgs),
    /// Write index (plumbing)
    WriteIndex(WriteIndexArgs),
    /// Encrypt files in index (plumbing)
    EncryptFiles(EncryptFilesArgs),
    /// Restore files from the index + encrypted data
    RestoreFiles(RestoreFilesArgs),
    /// List files in the index, optionally filtered
    ListFiles(ListFilesArgs),
    /// List files (alias for list-files)
    #[command(alias = "list")]
    Ls(ListFilesArgs),
    /// Manipulate tags on files
    Tagger(TaggerArgs),
    /// Print (debug) the index (plumbing)
    ReadIndex(ReadIndexArgs),
    // #[command(hide = true)]
    /// Probably old, needs removed at this point
    DebugIndex(DebugIndexArgs),
    /// Defrag consolidates encrypted blob files
    DefragBlobs(DefragBlobsArgs),
    /// Delete data from index and mark associated encrypted blobs as deleted
    DeleteFiles(DeleteFilesArgs),
    /// Search filenames, tags
    Search(SearchArgs),
    /// Status command, show changes not in index (and not encrypted?)
    Status(StatusArgs),

    /// Manage the blu agent daemon
    Agent(AgentArgs),

    /// Internal: run the agent daemon (not user-facing)
    #[command(name = "__agent-daemon", hide = true)]
    AgentDaemon,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct InitArgs {
    /// Directory to initialize as a blu vault
    pub dir: String,

    /// Import an existing age key file instead of generating a new one
    #[arg(long)]
    pub key_file: Option<String>,

    /// Do not encrypt the private key with a passphrase
    #[arg(long)]
    pub no_passphrase: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct AddArgs {
    pub add_paths: Vec<String>,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct SyncArgs {
    /// Paths to sync (defaults to current directory)
    pub paths: Vec<String>,

    /// Force write indexes even if no changes
    #[arg(long)]
    pub force: bool,

    /// Push indexes to remote backend after sync
    #[arg(long)]
    pub push: bool,

    /// Show verbose output
    #[arg(long, short)]
    pub verbose: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct PullArgs {
    /// Force overwrite local indexes even if they exist
    #[arg(long)]
    pub force: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct WriteIndexArgs {
    pub outfile: Option<String>,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct EncryptFilesArgs {
    #[arg(long)]
    pub force_write_index: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct RestoreFilesArgs {
    /// Restore files by hash prefix
    #[arg(long)]
    pub file_hashes: Vec<String>,

    /// Restore files matching path pattern (glob-style, e.g. "photos/*.jpg")
    #[arg(long)]
    pub path: Option<String>,

    /// Restore all files
    #[arg(long)]
    pub all: bool,

    /// Destination directory for restored files (default: original paths)
    #[arg(long)]
    pub to: Option<String>,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct ListFilesArgs {
    #[arg(long)]
    pub filter: Option<String>,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct TaggerArgs {
    #[command(flatten)]
    pub tag_action: TagAction,
    #[arg(long, required = true)]
    pub data_hash_filter: Vec<String>,
    #[arg(long, default_value = "false")]
    pub dry_run: bool,
}

#[allow(missing_docs)]
#[derive(clap::Args, Clone, Debug)]
#[group(required = true, multiple = false)]
pub struct TagAction {
    #[arg(long, conflicts_with = "remove_all_tags")]
    pub tags: Option<String>,

    #[arg(long, conflicts_with = "tags")]
    pub remove_all_tags: bool,
}

#[allow(missing_docs)]
#[derive(clap::Args, Clone, Debug)]
pub struct ReadIndexArgs {
    #[clap(value_enum)]
    pub index_type: IndexType,
    pub file: String,
}

#[allow(missing_docs)]
#[derive(clap::ValueEnum, Clone, Debug)]
pub enum IndexType {
    Plain,
    Blob,
    Tag,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct DebugIndexArgs {}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct DefragBlobsArgs {
    pub blob_index_path: String,

    #[arg(long, default_value = "false")]
    pub dry_run: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct DeleteFilesArgs {
    #[arg(long)]
    pub filter: Option<String>,
    #[arg(long, default_value = "false")]
    pub dry_run: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct SearchArgs {
    pub needle: String,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: AgentCommand,
}

/// Agent subcommands
#[derive(Debug, clap::Subcommand, Clone)]
pub enum AgentCommand {
    /// Show agent status (running, unlocked, timeout)
    Status,
    /// Stop the agent daemon
    Stop,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct StatusArgs {
    #[clap(value_enum)]
    #[arg(long = "type")]
    pub status_check_type: Option<StatusCheckType>,
}

/// Type of status check to run. Deep means hash every file, shallow will use
/// filenames(+sizes?) and assume nothing has changed.
#[derive(clap::ValueEnum, Clone, Debug)]
pub enum StatusCheckType {
    /// Deep check means hash every file
    Deep,
    /// Shallow check means use file path (+size?) to determine changes
    Shallow,
}
