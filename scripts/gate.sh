#!/usr/bin/env bash
# make check: every gate in one short throwaway root holding HOME, every XDG root, TMPDIR,
# CARGO_HOME and CARGO_TARGET_DIR (CLAUDE.md § Build and verification).
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
. ./pins.env
if [[ $(uname -s) != Linux ]]; then
    echo 'ERROR: make check runs on Linux only: holders are systemd user services' >&2
    exit 2
fi
if [[ $(uname -m) != x86_64 ]]; then
    echo "ERROR: pins.env pins the x86_64 Herdr artifact only; this machine is $(uname -m)" >&2
    exit 2
fi
# rustup's own home stays the caller's; only the build's HOME moves into the throwaway root.
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
root=$(mktemp -d /tmp/hs.XXXXXX)
trap 'echo "ERROR: make check failed; its logs and artifacts stay in $root" >&2' EXIT
export TMPDIR="$root" HOME="$root/home" XDG_CONFIG_HOME="$root/xdg-config" XDG_CACHE_HOME="$root/xdg-cache" \
    XDG_DATA_HOME="$root/xdg-data" XDG_STATE_HOME="$root/xdg-state" XDG_RUNTIME_DIR="$root/xdg-runtime" \
    CARGO_HOME="$root/cargo-home" CARGO_TARGET_DIR="$root/cargo-target"
export HERDR_SHIELD_BUILD_ROOT="$root" HERDR_SHIELD_TEST_ROOT="$root/tests" \
    HERDR_SHIELD_BIN="$CARGO_TARGET_DIR/debug/herdr-shield" HERDR_SHIELD_OLD_BIN="$root/old-herdr-shield"
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" \
    "$CARGO_HOME" "$CARGO_TARGET_DIR" "$HERDR_SHIELD_TEST_ROOT"
mkdir -m 700 -p "$XDG_RUNTIME_DIR"

herdr="$root/herdr-$HERDR_VERSION"
echo "== download Herdr $HERDR_VERSION"
curl -fsSL --retry 3 -o "$herdr" \
    "https://github.com/herdrdev/herdr/releases/download/v$HERDR_VERSION/herdr-linux-x86_64"
echo "$HERDR_SHA256  $herdr" | sha256sum --check --strict -
chmod 700 "$herdr"

step() {
    echo "== $*"
    "$@"
}
step cargo fmt --all -- --check
step cargo clippy --locked --all-targets --all-features -- -D warnings
# shortcut: the "old" executable is a release build of this same source, distinct from the debug
# build the tests run; once a prior release exists, build the previous tag here instead.
step cargo build --locked --release
cp "$CARGO_TARGET_DIR/release/herdr-shield" "$HERDR_SHIELD_OLD_BIN"
step cargo test --locked --all-targets --all-features
step bash scripts/checks.sh
step bash scripts/test-herdr.sh --herdr "$herdr"

trap - EXIT
rm -rf -- "$root"
echo 'PASS make check'
