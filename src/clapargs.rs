use clap::Parser;

/// Blu - de-duplicated filing system w/encrypted cloud backup
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub action: Action,
}

#[derive(Debug, clap::Subcommand, Clone)]
pub enum Action {
    Init,
    Add,
    Restore,
    #[command(hide = true)]
    PrintIndex,
}
