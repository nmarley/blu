#!/usr/bin/env bash
# Fail if Cargo.toml package version is behind the latest v* tag.
# Pure shell: no cargo, no network. Safe for pre-push and CI.

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "check-version: not inside a git work tree" >&2
    exit 1
}
cd "$ROOT"

fail() {
    echo "check-version: $*" >&2
    exit 1
}

cargo_version_from_file() {
    local file="$1"
    awk '
        /^\[package\]/ { in_pkg = 1; next }
        /^\[/ { in_pkg = 0 }
        in_pkg && /^version[[:space:]]*=/ {
            if (match($0, /"[0-9]+\.[0-9]+\.[0-9]+"/)) {
                print substr($0, RSTART + 1, RLENGTH - 2)
                exit
            }
        }
    ' "$file"
}

version_ge() {
    # True if $1 >= $2 under semver-ish sort -V.
    local higher
    higher="$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n 1)"
    [[ "$higher" == "$1" ]]
}

[[ -f Cargo.toml ]] || fail "Cargo.toml missing"

CARGO_VER="$(cargo_version_from_file Cargo.toml)"
[[ -n "$CARGO_VER" ]] || fail "could not parse package version from Cargo.toml"

LATEST_TAG="$(git tag -l 'v[0-9]*' --sort=-v:refname | head -n 1 || true)"
if [[ -n "$LATEST_TAG" ]]; then
    TAG_VER="${LATEST_TAG#v}"
    if ! version_ge "$CARGO_VER" "$TAG_VER"; then
        fail "Cargo.toml version ${CARGO_VER} is behind latest tag ${LATEST_TAG} (crate must be >= tag)"
    fi
fi

echo "check-version: ok (crate ${CARGO_VER}${LATEST_TAG:+, latest tag ${LATEST_TAG}})"
