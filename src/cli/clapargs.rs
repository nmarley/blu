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

    /// Unlock the agent (start if needed, cache passphrase)
    Unlock,
    /// Lock the agent (zeroize cached keys)
    Lock,

    /// Manage the blu agent daemon
    Agent(AgentArgs),

    /// Manage storage backends
    Backend(BackendArgs),

    /// Manage your global identity (mnemonic-based)
    Identity(IdentityArgs),

    /// Internal: run the agent daemon (not user-facing)
    #[command(name = "__agent-daemon", hide = true)]
    AgentDaemon,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct InitArgs {
    /// Directory to initialize as a blu vault
    pub dir: String,

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

    /// Use a specific named backend instead of the default
    #[arg(long)]
    pub backend: Option<String>,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct PullArgs {
    /// Force overwrite local indexes even if they exist
    #[arg(long)]
    pub force: bool,

    /// Pull from a specific named backend instead of the default
    #[arg(long)]
    pub backend: Option<String>,
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

    /// Restore from a specific named backend instead of the default
    #[arg(long)]
    pub backend: Option<String>,
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

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct IdentityArgs {
    #[command(subcommand)]
    pub command: IdentityCommand,
}

/// Identity subcommands
#[derive(Debug, clap::Subcommand, Clone)]
pub enum IdentityCommand {
    /// Generate a new mnemonic-based identity
    Init(IdentityInitArgs),
    /// Display the current identity's public key
    Show,
    /// Recover an identity from a BIP39 mnemonic
    Recover(IdentityRecoverArgs),
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct BackendArgs {
    #[command(subcommand)]
    pub command: BackendCommand,
}

/// Backend management subcommands
#[derive(Debug, clap::Subcommand, Clone)]
pub enum BackendCommand {
    /// Add a named storage backend
    Add(BackendAddArgs),
    /// List configured backends
    List(BackendListArgs),
    /// Remove a named backend
    Remove(BackendRemoveArgs),
    /// Set the default backend
    SetDefault(BackendSetDefaultArgs),
    /// Copy blobs from one backend to another
    Mirror(BackendMirrorArgs),
    /// Compare blob sets between two backends
    Diff(BackendDiffArgs),
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct BackendAddArgs {
    /// Name for the new backend
    pub name: String,

    /// Backend type (local, s3)
    #[arg(long = "type")]
    pub backend_type: String,

    /// Path for local backends
    #[arg(long)]
    pub path: Option<String>,

    /// S3 bucket name
    #[arg(long)]
    pub bucket: Option<String>,

    /// S3 key prefix
    #[arg(long)]
    pub prefix: Option<String>,

    /// AWS region
    #[arg(long)]
    pub region: Option<String>,

    /// Set as the default backend
    #[arg(long)]
    pub default: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct BackendRemoveArgs {
    /// Name of the backend to remove
    pub name: String,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct BackendSetDefaultArgs {
    /// Name of the backend to set as default
    pub name: String,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct BackendListArgs {
    /// Show blob counts per backend
    #[arg(long)]
    pub stats: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct BackendMirrorArgs {
    /// Source backend name
    #[arg(long)]
    pub from: String,

    /// Destination backend name
    #[arg(long)]
    pub to: String,

    /// Show what would be copied without transferring data
    #[arg(long)]
    pub dry_run: bool,

    /// Only mirror blobs referenced by files with this tag
    #[arg(long)]
    pub tag: Option<String>,

    /// Number of concurrent transfers
    #[arg(short, long, default_value = "16")]
    pub jobs: usize,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct BackendDiffArgs {
    /// First backend name
    #[arg(long)]
    pub from: String,

    /// Second backend name
    #[arg(long)]
    pub to: String,

    /// Number of concurrent checks
    #[arg(short, long, default_value = "16")]
    pub jobs: usize,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct IdentityInitArgs {
    /// Do not encrypt the identity file with a passphrase
    #[arg(long)]
    pub no_passphrase: bool,

    /// Overwrite an existing identity
    #[arg(long)]
    pub force: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct IdentityRecoverArgs {
    /// Do not encrypt the identity file with a passphrase
    #[arg(long)]
    pub no_passphrase: bool,

    /// Overwrite an existing identity
    #[arg(long)]
    pub force: bool,
}
