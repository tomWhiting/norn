#!/bin/sh
set -eu

iterations="${1:-20}"
output="${2:-docs/reviews/evidence/2026-07-14-mcp-startup.json}"
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
child_passed=0
child_failed=0
approval_passed=0
approval_failed=0
http_passed=0
http_failed=0
stdio_passed=0
stdio_failed=0
iteration=1
while [ "$iteration" -le "$iterations" ]; do
    if CARGO_INCREMENTAL=0 cargo test -p norn --lib \
        variant_child_can_widen_root_mcp_view_and_dispatch_beta \
        >"$log" 2>&1; then
        child_passed=$((child_passed + 1))
    else
        child_failed=$((child_failed + 1))
        cat "$log" >&2
    fi
    if CARGO_INCREMENTAL=0 cargo test -p norn-cli --lib \
        approved_project_server_connects_through_startup \
        >"$log" 2>&1; then
        approval_passed=$((approval_passed + 1))
    else
        approval_failed=$((approval_failed + 1))
        cat "$log" >&2
    fi
    if CARGO_INCREMENTAL=0 cargo test -p norn --lib \
        real_http_rejects_hostile_initialize_envelopes \
        >"$log" 2>&1; then
        http_passed=$((http_passed + 1))
    else
        http_failed=$((http_failed + 1))
        cat "$log" >&2
    fi
    if CARGO_INCREMENTAL=0 cargo test -p norn --lib \
        cancellation_after_write_invalidates_the_channel \
        >"$log" 2>&1; then
        stdio_passed=$((stdio_passed + 1))
    else
        stdio_failed=$((stdio_failed + 1))
        cat "$log" >&2
    fi
    iteration=$((iteration + 1))
done

mkdir -p "$(dirname "$output")"
printf '{\n  "iterations": %s,\n  "cases": [\n    {"filter": "variant_child_can_widen_root_mcp_view_and_dispatch_beta", "passed": %s, "failed": %s},\n    {"filter": "approved_project_server_connects_through_startup", "passed": %s, "failed": %s},\n    {"filter": "real_http_rejects_hostile_initialize_envelopes", "passed": %s, "failed": %s},\n    {"filter": "cancellation_after_write_invalidates_the_channel", "passed": %s, "failed": %s}\n  ]\n}\n' \
    "$iterations" \
    "$child_passed" "$child_failed" \
    "$approval_passed" "$approval_failed" \
    "$http_passed" "$http_failed" \
    "$stdio_passed" "$stdio_failed" >"$output"
cat "$output"
test "$child_failed" -eq 0
test "$approval_failed" -eq 0
test "$http_failed" -eq 0
test "$stdio_failed" -eq 0
