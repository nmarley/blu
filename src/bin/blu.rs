#![allow(clippy::uninlined_format_args)]

use clap::Parser;
use simplelog::*;
use std::env;
use std::path::{Path, PathBuf};

use blu::cli::{self, clapargs, helpers};
use blu::error::BluError;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

pub async fn run() -> Result<(), BluError> {
    CombinedLogger::init(vec![TermLogger::new(
        LevelFilter::Debug,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )])
    .unwrap();

    let args = clapargs::Args::parse();

    // Set global no-passphrase flag from CLI args
    helpers::set_no_passphrase(args.no_passphrase);

    // Commands that do not require a blu repository
    match &args.action {
        clapargs::Action::Agent(a) => return cli::agent(a.clone()),
        clapargs::Action::Identity(a) => return cli::identity(a.clone()),
        clapargs::Action::Lock => return cli::lock(),
        clapargs::Action::Unlock => return cli::unlock(),
        clapargs::Action::AgentDaemon => {
            let paths = blu::agent::AgentPaths::resolve()?;
            return blu::agent::run_daemon(&paths);
        }
        _ => {}
    }

    let blu_basedir = match find_blu_basedir(&args.bludir) {
        Some(dir) => dir,
        None => {
            match args.action {
                // init can run without all these other checks ...
                clapargs::Action::Init(a) => return cli::init(a),
                _ => {
                    return Err(BluError::NotARepository);
                }
            };
        }
    };

    let abspath = match std::fs::canonicalize(&blu_basedir) {
        Ok(path) => path,
        Err(_e) => {
            // likely won't ever happen ...
            return Err(BluError::Internal(format!(
                "fatal: unable to get absolute path for {:?}",
                &blu_basedir
            )));
        }
    };

    // move into the basedir for all operations, like `git -C <dir>`
    if let Err(e) = env::set_current_dir(&abspath) {
        return Err(BluError::Internal(format!(
            "unable to chdir to '{:?}': {}",
            &abspath, e
        )));
    }

    // TODO: Should key(s) be read and stored here in some kind of state or context?

    match args.action {
        clapargs::Action::Add(a) => cli::add(a).await,
        clapargs::Action::Backend(a) => cli::backend(a).await,
        clapargs::Action::DebugIndex(a) => cli::debug_index(a),
        clapargs::Action::DefragBlobs(a) => cli::defrag_blobs(a).await,
        clapargs::Action::DeleteFiles(a) => cli::delete_files(a).await,
        clapargs::Action::EncryptFiles(a) => cli::encrypt_files(a).await,
        clapargs::Action::Init(a) => cli::init(a),
        clapargs::Action::ListFiles(a) => cli::list_files(a),
        clapargs::Action::Ls(a) => cli::list_files(a),
        clapargs::Action::Pull(a) => cli::pull(a).await,
        clapargs::Action::ReadIndex(a) => cli::read_index(a),
        clapargs::Action::RestoreFiles(a) => cli::restore_files(a).await,
        clapargs::Action::Search(a) => cli::search(a),
        clapargs::Action::Status(a) => cli::status(a),
        clapargs::Action::Sync(a) => cli::sync(a).await,
        clapargs::Action::Tagger(a) => cli::tagger(a).await,
        clapargs::Action::WriteIndex(a) => cli::write_index(a),
        clapargs::Action::Serve(a) => blu::serve::serve(a.bind).await,
        // These are dispatched above, before basedir resolution
        clapargs::Action::Agent(_)
        | clapargs::Action::AgentDaemon
        | clapargs::Action::Identity(_)
        | clapargs::Action::Lock
        | clapargs::Action::Unlock => {
            unreachable!()
        }
    }
}

fn find_blu_basedir<P: AsRef<Path>>(dest: P) -> Option<PathBuf> {
    let mut d = dest.as_ref().to_path_buf();
    if d.join(".blu").exists() {
        return Some(d);
    }

    while let Some(parent) = d.parent() {
        if parent.join(".blu").exists() {
            return Some(parent.to_path_buf());
        }
        d = parent.to_path_buf();
    }

    None
}

#[cfg(test)]
mod test {
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    macro_rules! test_find_blu_basedir {
        ($name:ident, $in:expr, $out:expr) => {
            #[test]
            fn $name() {
                let root = tempdir().unwrap();
                let path = root.path().join($in);
                let rv = fs::create_dir_all(&path);
                assert!(rv.is_ok(), "Couldn't create test directories");

                let expected = $out.map(|pb| root.path().join(pb).to_path_buf());

                let bludir = super::find_blu_basedir(&path);
                assert_eq!(bludir, expected);
            }
        };
    }

    test_find_blu_basedir!(blu_basedir1, "sub1/.blu", Some(PathBuf::from("sub1")));
    test_find_blu_basedir!(
        blu_basedir2,
        "sub1/sub2/sub3/.blu",
        Some(PathBuf::from("sub1/sub2/sub3"))
    );
    test_find_blu_basedir!(
        blu_basedir3,
        "sub1/.blu/sub2/sub3",
        Some(PathBuf::from("sub1"))
    );
    // return the innermost nested instance, like git does
    test_find_blu_basedir!(
        blu_basedir4,
        "sub1/.blu/sub2/sub3/.blu",
        Some(PathBuf::from("sub1/.blu/sub2/sub3"))
    );
    test_find_blu_basedir!(blu_basedir5, "sub1/sub2/sub3", None::<PathBuf>);
}
