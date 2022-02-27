# TODO

Probably should use <https://crates.io/crates/age> and not shell out to rage tool.

See also: [https://rust-cli.github.io/book/index.html](Command line apps in Rust).

Clap: <https://docs.rs/clap/latest/clap/>

De-facto most use SQLite library in Rust: <https://rust-lang-nursery.github.io/rust-cookbook/database/sqlite.html>

Multihash for hashing <https://github.com/multiformats/rust-multihash>

## Design

```sh
blu init /  # should not be allowed
# <== root filesystem backups are not supported due to the size and amount of OS backups, please extra space is needed for encryption and de-duplication. Please use a custom directory for specific files

blu init .  ./data  # should not be allowed
# <== please use only 1 directory for a blu installation (you can have multiple, but they will need to be managed separately)
```
