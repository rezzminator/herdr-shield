#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
if [[ $# != 2 || $1 != --herdr || ! -x $2 ]]; then
    echo 'ERROR: usage: bash scripts/test-herdr.sh --herdr <pinned-v0.9.5-binary>' >&2
    exit 2
fi
: "${HERDR_SHIELD_BUILD_ROOT:?set an absolute private throwaway build root}"
: "${HERDR_SHIELD_BIN:?set the built herdr-shield binary path}"
/usr/bin/python3 -I -B tests/herdr/real.py --herdr "$2" --keeper "$HERDR_SHIELD_BIN" --root "$HERDR_SHIELD_BUILD_ROOT"
exec /usr/bin/python3 -I -B tests/herdr/recovery.py --herdr "$2" --keeper "$HERDR_SHIELD_BIN" --root "$HERDR_SHIELD_BUILD_ROOT"
