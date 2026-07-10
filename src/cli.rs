/// clapargs for CLI args
pub mod clapargs;
/// helper functions for CLI commands
pub mod helpers;
/// unified CLI output structs for all CLI modules
pub mod output;

mod add;
mod agent_cmd;
mod backend_cmd;
mod defrag_blobs;
mod delete_files;
mod doctor;
mod encrypt_files;
mod identity_cmd;
mod init;
mod list_files;
mod open;
mod pull;
mod read_index;
mod restore_files;
mod search;
#[cfg(test)]
mod smoke;
mod status;
mod sync;
mod tagger;
mod write_index;

pub use add::add;
pub use agent_cmd::agent;
pub use agent_cmd::lock;
pub use agent_cmd::unlock;
pub use backend_cmd::backend;
pub use defrag_blobs::defrag_blobs;
pub use delete_files::delete_files;
pub use doctor::doctor;
pub use encrypt_files::encrypt_files;
pub use identity_cmd::identity;
pub use identity_cmd::{global_identity_age_path, load_global_identity, IdentityMeta};
pub use init::{init, init_vault, InitVaultParams, InitVaultResult};
pub use list_files::list_files;
pub use open::{open, open_vault, OpenVaultParams};
pub use pull::pull;
pub use read_index::read_index;
pub use restore_files::restore_files;
pub use search::search;
pub use status::status;
pub use sync::sync;
pub use tagger::tagger;
pub use write_index::write_index;

pub(crate) use write_index::{check_outfile_writable, write_index_file};
