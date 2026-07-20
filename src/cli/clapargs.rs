use clap::{ArgAction, Parser};

/// Encrypted, content-addressed file vault (git-like catalog + checkout)
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Vault directory to operate in, like `git -C`
    #[arg(long, default_value = ".")]
    pub bludir: String,

    /// Do not prompt for passphrase (fail if key is encrypted)
    #[arg(long, global = true)]
    pub no_passphrase: bool,

    /// Increase log verbosity (-v info, -vv debug)
    #[arg(short = 'v', long, action = ArgAction::Count, global = true)]
    pub verbose: u8,

    /// The subcommand to run
    #[command(subcommand)]
    pub action: Action,
}

/// The possible subcommands to be run from blu-cli
#[derive(Debug, clap::Subcommand, Clone)]
pub enum Action {
    /// Create a new vault
    Init(InitArgs),
    /// Open an existing vault from a remote backend
    Open(OpenArgs),
    /// Index paths, encrypt, and publish to the vault backend
    Backup(BackupArgs),
    /// Fetch and merge remote catalog indexes (no plaintext)
    Pull(PullArgs),
    /// Write index (plumbing)
    #[command(hide = true)]
    WriteIndex(WriteIndexArgs),
    /// Encrypt files in index (plumbing)
    #[command(hide = true)]
    EncryptFiles(EncryptFilesArgs),
    /// Materialize plaintext from the catalog and encrypted blobs
    Restore(RestoreArgs),
    /// Initiate or report S3 archive restores for vault blobs
    Thaw(ThawArgs),
    /// List catalog entries, optionally filtered
    ListFiles(ListFilesArgs),
    /// List catalog entries (alias for list-files)
    #[command(alias = "list")]
    Ls(ListFilesArgs),
    /// Manipulate tags on files
    Tagger(TaggerArgs),
    /// Print (debug) the index (plumbing)
    #[command(hide = true)]
    ReadIndex(ReadIndexArgs),
    /// Repack partially-dead encrypted blob files
    DefragBlobs(DefragBlobsArgs),
    /// Tombstone catalog entries and cascade blob cleanup
    Rm(RmArgs),
    /// Search filenames and tags in the catalog
    Search(SearchArgs),
    /// Show working tree vs catalog vs remote
    Status(StatusArgs),
    /// Run vault health diagnostics
    Doctor(DoctorArgs),
    /// Start local HTTP server (S3-compatible API for the vault)
    Serve(ServeArgs),

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

/// Open an existing vault whose indexes and KEK store live on a backend.
///
/// Creates a local `.blu/` pointing at the given backend, then pulls
/// the UK-wrapped KEK store and encrypted indexes. Does not generate a
/// new KEK. Requires a global identity (`blu identity recover`).
#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct OpenArgs {
    /// Directory to open the vault into (created if missing)
    #[arg(long, default_value = ".")]
    pub dir: String,

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

    /// Name for the backend in config
    #[arg(long, default_value = "default")]
    pub backend_name: String,

    /// Do not prompt for passphrase (fail if identity is encrypted)
    #[arg(long)]
    pub no_passphrase: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct BackupArgs {
    /// Paths to back up (defaults to current directory)
    pub paths: Vec<String>,

    /// Force write indexes even if no changes
    #[arg(long)]
    pub force: bool,

    /// Use a specific named backend instead of the default
    #[arg(long)]
    pub backend: Option<String>,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct PullArgs {
    /// Discard local indexes and take the remote copy only (hard reset)
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
pub struct RestoreArgs {
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

    /// Initiate archive restore for cold blobs before reading
    #[arg(long)]
    pub thaw: bool,

    /// Wait until cold blobs are readable (implies --thaw)
    #[arg(long)]
    pub wait: bool,

    /// Use Standard restore tier instead of Bulk (with --thaw/--wait)
    #[arg(long)]
    pub standard: bool,

    /// Max hours to wait when --wait is set
    #[arg(long)]
    pub timeout_hours: Option<u64>,
}

/// Initiate or report archive restores for content-addressed blobs.
#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct ThawArgs {
    /// Select files by hash prefix
    #[arg(long)]
    pub file_hashes: Vec<String>,

    /// Select files matching path pattern (glob-style)
    #[arg(long)]
    pub path: Option<String>,

    /// Select all catalog files
    #[arg(long)]
    pub all: bool,

    /// Only report cold status; do not initiate RestoreObject
    #[arg(long)]
    pub status: bool,

    /// Wait until selected blobs are readable
    #[arg(long)]
    pub wait: bool,

    /// Use Standard restore tier instead of Bulk
    #[arg(long)]
    pub standard: bool,

    /// Days for classic Glacier temporary copy (ignored for Intelligent-Tiering)
    #[arg(long)]
    pub days: Option<u32>,

    /// Max hours to wait when --wait is set
    #[arg(long)]
    pub timeout_hours: Option<u64>,

    /// Use a specific named backend instead of the default
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
pub struct DefragBlobsArgs {
    /// Show what would be repacked without modifying anything
    #[arg(long, default_value = "false")]
    pub dry_run: bool,

    /// Use a specific named backend instead of the default
    #[arg(long)]
    pub backend: Option<String>,

    /// Rewrite all legacy v2 blobs into the v3 segmented format
    /// instead of repacking partially-dead blobs
    #[arg(long, default_value = "false")]
    pub upgrade_format: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct RmArgs {
    /// Filter by hash prefix, path substring, or tag (required unless --all)
    #[arg(long)]
    pub filter: Option<String>,

    /// Delete all files from the index
    #[arg(long)]
    pub all: bool,

    /// Show what would be deleted without modifying indexes
    #[arg(long, default_value = "false")]
    pub dry_run: bool,

    /// Repack partially-dead blobs inline after deletion
    #[arg(long)]
    pub scrub: bool,

    /// Delete blobs from a specific named backend instead of the default
    #[arg(long)]
    pub backend: Option<String>,
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

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct DoctorArgs {}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct ServeArgs {
    /// Bind address for the HTTP server (default: 127.0.0.1:7777)
    #[arg(long)]
    pub bind: Option<String>,

    /// Number of decrypted blobs to keep in the in-memory LRU cache
    /// (default: 10, ~64 MiB per entry at default chunk/blob sizing).
    #[arg(long)]
    pub cache_blobs: Option<usize>,
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
    /// Rename a named backend
    Rename(BackendRenameArgs),
    /// Set the default backend
    SetDefault(BackendSetDefaultArgs),
    /// Copy blobs from one backend to another
    Mirror(BackendMirrorArgs),
    /// Compare blob sets between two backends
    Diff(BackendDiffArgs),
    /// S3 Intelligent-Tiering helpers (print recommended config)
    #[command(name = "intelligent-tiering")]
    IntelligentTiering(BackendIntelligentTieringArgs),
}

/// Intelligent-Tiering subcommands under `blu backend intelligent-tiering`
#[derive(Debug, clap::Subcommand, Clone)]
pub enum IntelligentTieringCommand {
    /// Print recommended archive configuration JSON (operator applies it)
    Print(BackendItPrintArgs),
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct BackendIntelligentTieringArgs {
    #[command(subcommand)]
    pub command: IntelligentTieringCommand,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct BackendItPrintArgs {
    /// Named backend to read bucket/prefix/region from (default: vault default)
    #[arg(long)]
    pub backend: Option<String>,

    /// Configuration id (default: blu-blobs-deep-archive)
    #[arg(long)]
    pub id: Option<String>,

    /// Days of no access before Deep Archive Access (default: 365, min 180)
    #[arg(long)]
    pub days: Option<u32>,

    /// Override S3 key prefix in the filter (default: backend prefix if S3)
    #[arg(long)]
    pub prefix: Option<String>,
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
pub struct BackendRenameArgs {
    /// Current backend name
    pub old: String,

    /// New backend name
    pub new: String,
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
    #[arg(long, default_value = "16", value_parser = clap::value_parser!(u16).range(1..))]
    pub jobs: u16,
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
    #[arg(long, default_value = "16", value_parser = clap::value_parser!(u16).range(1..))]
    pub jobs: u16,
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
