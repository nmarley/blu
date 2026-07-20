#!/usr/bin/env bash
# Headless end-to-end smoke: drive the whole CLI pipeline (identity
# init --yes, vault init, backup, restore, doctor) with no TTY, using
# BLU_PASSPHRASE for every passphrase prompt.
#
# Everything is sandboxed: XDG dirs and HOME point at a temp dir, and
# BLU_NO_BIOMETRIC keeps the run away from the macOS keychain. A real
# identity under ~/.local/share/blu is never touched.

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "e2e-passphrase-smoke: not inside a git work tree" >&2
    exit 1
}
cd "$ROOT"

fail() {
    echo "e2e-passphrase-smoke: $*" >&2
    exit 1
}

cargo build --quiet
BLU_BIN="$ROOT/target/debug/blu"
[ -x "$BLU_BIN" ] || fail "blu binary not found at $BLU_BIN"

TMP="$(mktemp -d)"
cleanup() {
    # Stop the sandboxed agent before deleting its socket dir.
    HOME="$TMP/home" \
    XDG_CONFIG_HOME="$TMP/config" \
    XDG_DATA_HOME="$TMP/data" \
    XDG_STATE_HOME="$TMP/state" \
    XDG_RUNTIME_DIR="$TMP/run" \
        "$BLU_BIN" agent stop >/dev/null 2>&1 || true
    rm -rf "$TMP"
}
trap cleanup EXIT

export HOME="$TMP/home"
export XDG_CONFIG_HOME="$TMP/config"
export XDG_DATA_HOME="$TMP/data"
export XDG_STATE_HOME="$TMP/state"
export XDG_RUNTIME_DIR="$TMP/run"
export BLU_PASSPHRASE="e2e-smoke-passphrase"
export BLU_NO_BIOMETRIC=1
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR"

echo "==> blu identity init --yes"
"$BLU_BIN" identity init --yes >/dev/null

# The identity file must be age-encrypted on disk, never plaintext.
grep -q '^age-encryption.org' "$XDG_DATA_HOME/blu/identity.age" \
    || fail "identity.age is not encrypted"

VAULT="$TMP/vault"
mkdir -p "$VAULT/sub"
echo "most triumphant e2e content" > "$VAULT/wyld-stallyns.txt"
echo "bogus content" > "$VAULT/sub/bogus.txt"

echo "==> blu init"
"$BLU_BIN" init "$VAULT" >/dev/null

echo "==> blu backup"
( cd "$VAULT" && "$BLU_BIN" backup >/dev/null )

OUT="$TMP/restored"
mkdir -p "$OUT"
echo "==> blu restore --all"
( cd "$VAULT" && "$BLU_BIN" restore --all --to "$OUT" >/dev/null )

echo "==> blu doctor"
( cd "$VAULT" && "$BLU_BIN" doctor >/dev/null )

diff "$VAULT/wyld-stallyns.txt" "$OUT/wyld-stallyns.txt" >/dev/null \
    || fail "restored wyld-stallyns.txt differs"
diff "$VAULT/sub/bogus.txt" "$OUT/sub/bogus.txt" >/dev/null \
    || fail "restored sub/bogus.txt differs"

echo "e2e-passphrase-smoke: OK"
