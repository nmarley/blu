use clap::Parser;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[clap(version = "0.1")]
struct Args {
    /// Number of crawlers to run in parallel
    #[clap(short = 't', long, default_value_t = 96)]
    pub num_crawlers: u32,

    /// Number of DNS server threads
    #[clap(short, long, default_value = "4")]
    pub dns_threads: u32,

    /// UDP port to listen on
    #[clap(short, long, default_value = "53")]
    pub port: u16,

    /// Number of seconds to sleep before printing stats
    #[clap(short, long, default_value = "1")]
    pub stats_sleep_seconds: u16,

    /// Wipe list of banned nodes
    #[clap(long)]
    pub wipeban: bool,

    /// Tor proxy IP/Port
    #[clap(short = 'o', long = "onion", value_name = "ip:port")]
    pub tor: Option<String>,

    /// Flag filter (combine network filters with bitwise &)
    #[clap(short = 'w', long)]
    pub filter: Option<u32>,
}

// also: consider an internal webserver which serves up the UI for blu
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Args = Args::parse();

    if args.num_crawlers < 1 || args.num_crawlers > 999 {
        args.num_crawlers = 96; // how to get default here?
    }
    println!("args: {:?}", args);

    // let key = read-key-from-.blu/metadata.json;
    // decrypt somehow?

    // let conn = db.connection();
    let dir = ".";
    index(dir)
    // sync()
}

// walk the dir and hash all regular files
// ignore block/char specials
//
// TODO: need to accept an SQLite3 connection for metadata writes
fn index(base_dir: &str) -> Result<(), Box<dyn std::error::Error>> {
    for entry in WalkDir::new(base_dir).into_iter().filter_map(|e| e.ok()) {
        // TODO: allow symlinks?
        // if !entry.file_type().is_file() {
        //     continue;
        // }
        // count += 1;

        // let metadata = fs::metadata(entry.path())?;
        // let size = metadata.len();
        // // println!("{:?}: {:?} bytes", entry.path(), size);

        // if size > max {
        //     max = size;
        //     biggest = entry.path().to_path_buf();
        // }
        // if size < min {
        //     min = size;
        //     smallest = entry.path().to_path_buf();
        // }
    }

    Ok(())
}

// fn sync() -> Result<(), Box<dyn std::error::Error>> {
//     Err("something didn't work".into())
//     // Ok(())
// }
