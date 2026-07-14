#!/bin/sh
set -eu

iterations="${1:-20}"
output="${2:-docs/reviews/evidence/2026-07-14-mcp-live.json}"
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
filters='pre_subscription_change_is_refreshed
change_during_refresh_schedules_the_latest_revision
removed_client_is_not_retained_by_its_watcher
public_root_update_cannot_split_contextual_tool_call
new_child_observes_replaced_pool_while_existing_child_keeps_lease
dropping_transport_returns_retained_descriptor_capacity
live_definition_secrets_never_reach_file_backed_history'

results=''
for filter in $filters; do
    passed=0
    failed=0
    iteration=1
    while [ "$iteration" -le "$iterations" ]; do
        package='norn'
        if [ "$filter" = 'live_definition_secrets_never_reach_file_backed_history' ]; then
            package='norn-tui'
        fi
        if CARGO_INCREMENTAL=0 cargo test -p "$package" --lib "$filter" >"$log" 2>&1; then
            passed=$((passed + 1))
        else
            failed=$((failed + 1))
            cat "$log" >&2
        fi
        iteration=$((iteration + 1))
    done
    entry="    {\"filter\": \"$filter\", \"passed\": $passed, \"failed\": $failed}"
    if [ -n "$results" ]; then
        results="$results,
$entry"
    else
        results="$entry"
    fi
done

mkdir -p "$(dirname "$output")"
printf '{\n  "iterations": %s,\n  "cases": [\n%s\n  ]\n}\n' \
    "$iterations" "$results" >"$output"
cat "$output"
if grep -q '"failed": [^0]' "$output"; then
    exit 1
fi
