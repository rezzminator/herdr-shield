#!/bin/sh
set -eu
umask 077
printf '%s\n' "$$" > "$HOME/dummy.pid"
counter=0
if [ -n "${HERDR_SHIELD_INNER:-}" ]; then
    printf '\033[2J\033[HREADY\n'
else
    printf '\r\nPLAIN_READY\n'
fi
while IFS= read -r line; do
    case "$line" in
        tick) counter=$((counter + 1)); printf '\033[2;3HCOUNTER=%s' "$counter" ;;
        attach\ *) exec "$HERDR_SHIELD_TEST_BIN" attach "${line#attach }" ;;
        agent) exec /usr/bin/python3 -I "$HOME/dummy-agent.py" ;;
        flood) index=0; while [ "$index" -lt 5000 ]; do printf 'bounded-output\r\n'; index=$((index + 1)); done ;;
        exit) exit 0 ;;
    esac
done
