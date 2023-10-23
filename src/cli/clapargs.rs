use clap::Parser;

/// Blu - de-duplicated file archival system w/encrypted cloud backup
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
    Init(InitArgs),
    /// Write index
    WriteIndex(WriteIndexArgs),
    /// Encrypt files in index
    EncryptFiles(EncryptFilesArgs),
    /// Restore files from the index + encrypted data
    RestoreFiles(RestoreFilesArgs),
    /// List files in the index, optionally filtered
    ListFiles(ListFilesArgs),
    /// Still got the ol' tagger on it, see?
    Tagger(TaggerArgs),
    /// Print (debug) the index
    ReadIndex(ReadIndexArgs),
    /// Probably old, needs removed at this point
    DebugIndex(DebugIndexArgs),
    // #[command(hide = true)]
    // /// Print (debug) the index. Deprecated.
    // PrintIndex,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct InitArgs {
    pub dir: String,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct WriteIndexArgs {
    pub dir: String,
    pub outfile: Option<String>,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct EncryptFilesArgs {
    pub dir: String,
    #[arg(long)]
    pub force_write_index: bool,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct RestoreFilesArgs {
    pub dir: String,
    pub restore_paths: Vec<String>,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct ListFilesArgs {
    pub dir: String,
    #[arg(long)]
    pub filter: Option<String>,
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct TaggerArgs {
    // dir OR file -- will probably change this to use `-C` option (like git)
    pub dest: String,
    #[command(flatten)]
    pub tag_action: TagAction,
    #[arg(long)]
    pub data_hash_filter: Option<String>,
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
pub struct DebugIndexArgs {
    pub dir: String,
}
