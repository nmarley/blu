use clap::Parser;

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
