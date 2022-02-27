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

## Web UI

If a web-ui is added, probably would like to use Actix-Web. A new version was just released recently (as of 2022-02-27).


## TODOs

- Should there be a .bluignore, similar to .gitignore? Or within .blu, e.g. .blu/ignore?
