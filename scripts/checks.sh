#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
/usr/bin/python3 -I -B scripts/checks.py
exec /usr/bin/python3 -I -B scripts/test-gates.py
