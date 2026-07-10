#! /bin/bash
# Install blu into the cargo bin dir (canonical Rust install path).
# On macOS, re-apply a local ad-hoc codesign after install so taskgated
# does not SIGKILL linker-signed copies (seen on macOS 26 + recent rustc).
#
# Usage:
#   bash scripts/install-local.sh
#   bash scripts/install-local.sh --debug   # install debug profile

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "install-local: not inside a git work tree" >&2
    exit 1
}
cd "$ROOT"

profile_args=(--path . --force)
if [[ "${1:-}" == "--debug" ]]; then
    profile_args+=(--debug)
    shift
elif [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    echo "Usage: bash scripts/install-local.sh [--debug]"
    exit 0
fi

echo "install-local: cargo install ${profile_args[*]}"
cargo install "${profile_args[@]}"

cargo_home="${CARGO_HOME:-$HOME/.cargo}"
bin="${cargo_home}/bin/blu"

if [[ ! -f "$bin" ]]; then
    echo "install-local: expected binary at $bin (not found)" >&2
    exit 1
fi

if [[ "$(uname -s)" == "Darwin" ]]; then
    echo "install-local: ad-hoc codesign $bin"
    codesign -s - --force --timestamp=none "$bin"
    codesign -vv --strict "$bin"
fi

echo "install-local: $($bin --version) -> $bin"
