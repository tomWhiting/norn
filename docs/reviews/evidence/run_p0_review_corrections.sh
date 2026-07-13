#!/bin/sh
set -eu

iterations="${1:-20}"
output="${2:-docs/reviews/evidence/2026-07-13-p0-review-corrections.json}"
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
deadline_passed=0
deadline_failed=0
repair_passed=0
repair_failed=0
iteration=1
while [ "$iteration" -le "$iterations" ]; do
    if CARGO_INCREMENTAL=0 cargo test -p norn-cli --test index_lock_deadline \
        held_lock_times_out_typed_with_config_derived_deadline >"$log" 2>&1; then
        deadline_passed=$((deadline_passed + 1))
    else
        deadline_failed=$((deadline_failed + 1))
        cat "$log" >&2
    fi
    if CARGO_INCREMENTAL=0 cargo test -p norn --lib \
        resume_repair_drops_stale_anchor_for_first_healed_request >"$log" 2>&1; then
        repair_passed=$((repair_passed + 1))
    else
        repair_failed=$((repair_failed + 1))
        cat "$log" >&2
    fi
    iteration=$((iteration + 1))
done

mkdir -p "$(dirname "$output")"
printf '{\n  "iterations": %s,\n  "cases": [\n    {"filter": "held_lock_times_out_typed_with_config_derived_deadline", "passed": %s, "failed": %s},\n    {"filter": "resume_repair_drops_stale_anchor_for_first_healed_request", "passed": %s, "failed": %s}\n  ]\n}\n' \
    "$iterations" \
    "$deadline_passed" "$deadline_failed" \
    "$repair_passed" "$repair_failed" >"$output"
cat "$output"
test "$deadline_failed" -eq 0
test "$repair_failed" -eq 0
