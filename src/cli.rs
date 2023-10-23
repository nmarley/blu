/// clapargs for CLI args
pub mod clapargs;
mod debug_index;
mod encrypt_files;
mod init;
mod list_files;
mod read_index;
mod restore_files;
mod tagger;
mod write_index;

pub use debug_index::debug_index;
pub use encrypt_files::encrypt_files;
pub use init::init;
pub use list_files::list_files;
pub use read_index::read_index;
pub use restore_files::restore_files;
pub use tagger::tagger;
pub use write_index::write_index;
