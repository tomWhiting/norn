#!/bin/sh
set -eu

iterations="${1:-20}"
output="${2:-docs/reviews/evidence/2026-07-14-p0-pty.json}"
case "$iterations" in
    ''|*[!0-9]*)
        echo "iterations must be an integer of at least 20" >&2
        exit 2
        ;;
esac
if [ "$iterations" -lt 20 ]; then
    echo "iterations must be at least 20" >&2
    exit 2
fi

log="$(mktemp)"
trap 'rm -f "$log"' EXIT HUP INT TERM
passed=0
failed=0
iteration=1
while [ "$iteration" -le "$iterations" ]; do
    if CARGO_INCREMENTAL=0 cargo test -p norn-tui --test pty_smoke >"$log" 2>&1; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
        cat "$log" >&2
    fi
    iteration=$((iteration + 1))
done

mkdir -p "$(dirname "$output")"
printf '{\n  "iterations": %s,\n  "cases": [\n    {"filter": "norn-tui pty_smoke", "passed": %s, "failed": %s}\n  ]\n}\n' \
    "$iterations" "$passed" "$failed" >"$output"
cat "$output"
test "$failed" -eq 0
