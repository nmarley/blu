use clap::Parser;

/// blu - de-duplicated file archival system w/encrypted cloud backup
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// The target folder for blu to run/operate in, like `git -C`
    #[arg(long, default_value = ".")]
    pub bludir: String,

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
    /// Manipulate tags on files
    Tagger(TaggerArgs),
    /// Print (debug) the index
    ReadIndex(ReadIndexArgs),
    // #[command(hide = true)]
    /// Probably old, needs removed at this point
    DebugIndex(DebugIndexArgs),
    /// Defrag consolidates encrypted blob files
    DefragBlobs(DefragBlobsArgs),
    /// Delete data from index and mark associated encrypted blobs as deleted
    DeleteFiles(DeleteFilesArgs),
    /// Full-text search on filenames, maybe tags (TBD)
    SearchFiles(SearchFilesArgs),
}

#[allow(missing_docs)]
#[derive(Parser, Debug, Clone)]
pub struct InitArgs {
    pub dir: String,
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
    pub restore_paths: Vec<String>,
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
    // dir OR file -- will probably change this to use `-C` option (like git)
    // pub dest: Vec<String>,
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
pub struct SearchFilesArgs {
    pub needle: String,
}
