#!/bin/sh
set -eu

iterations="${1:-20}"
output="${2:-docs/reviews/evidence/2026-07-14-descriptor-admission.json}"
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
retention_passed=0
retention_failed=0
cancellation_passed=0
cancellation_failed=0
migration_passed=0
migration_failed=0
adoption_passed=0
adoption_failed=0
line_log_passed=0
line_log_failed=0
task_transaction_passed=0
task_transaction_failed=0
iteration=1
while [ "$iteration" -le "$iterations" ]; do
    if CARGO_INCREMENTAL=0 cargo test -p norn --lib descriptor_retention -- --nocapture >"$log" 2>&1; then
        retention_passed=$((retention_passed + 1))
    else
        retention_failed=$((retention_failed + 1))
        cat "$log" >&2
    fi
    if CARGO_INCREMENTAL=0 cargo test -p norn --lib \
        cancelling_foreground_shell_releases_child_drains_and_capacity -- --nocapture \
        >"$log" 2>&1; then
        cancellation_passed=$((cancellation_passed + 1))
    else
        cancellation_failed=$((cancellation_failed + 1))
        cat "$log" >&2
    fi
    if CARGO_INCREMENTAL=0 cargo test -p norn --lib \
        timeout_with_partial_output_migrates_and_seeds_spool -- --nocapture \
        >"$log" 2>&1; then
        migration_passed=$((migration_passed + 1))
    else
        migration_failed=$((migration_failed + 1))
        cat "$log" >&2
    fi
    if CARGO_INCREMENTAL=0 cargo test -p norn --lib \
        cancellation_before_adoption_commit_kills_the_process -- --nocapture \
        >"$log" 2>&1; then
        adoption_passed=$((adoption_passed + 1))
    else
        adoption_failed=$((adoption_failed + 1))
        cat "$log" >&2
    fi
    if CARGO_INCREMENTAL=0 cargo test -p norn --lib \
        concurrent_writers_preserve_complete_records -- --nocapture \
        >"$log" 2>&1; then
        line_log_passed=$((line_log_passed + 1))
    else
        line_log_failed=$((line_log_failed + 1))
        cat "$log" >&2
    fi
    if CARGO_INCREMENTAL=0 cargo test -p norn --lib \
        exact_weight_supports_nested_work_without_nested_admission -- --nocapture \
        >"$log" 2>&1; then
        task_transaction_passed=$((task_transaction_passed + 1))
    else
        task_transaction_failed=$((task_transaction_failed + 1))
        cat "$log" >&2
    fi
    iteration=$((iteration + 1))
done

mkdir -p "$(dirname "$output")"
printf '{\n  "iterations": %s,\n  "cases": [\n    {"filter": "descriptor_retention", "passed": %s, "failed": %s},\n    {"filter": "cancelling_foreground_shell_releases_child_drains_and_capacity", "passed": %s, "failed": %s},\n    {"filter": "timeout_with_partial_output_migrates_and_seeds_spool", "passed": %s, "failed": %s},\n    {"filter": "cancellation_before_adoption_commit_kills_the_process", "passed": %s, "failed": %s},\n    {"filter": "concurrent_writers_preserve_complete_records", "passed": %s, "failed": %s},\n    {"filter": "exact_weight_supports_nested_work_without_nested_admission", "passed": %s, "failed": %s}\n  ]\n}\n' \
    "$iterations" \
    "$retention_passed" "$retention_failed" \
    "$cancellation_passed" "$cancellation_failed" \
    "$migration_passed" "$migration_failed" \
    "$adoption_passed" "$adoption_failed" \
    "$line_log_passed" "$line_log_failed" \
    "$task_transaction_passed" "$task_transaction_failed" >"$output"
cat "$output"
test "$retention_failed" -eq 0
test "$cancellation_failed" -eq 0
test "$migration_failed" -eq 0
test "$adoption_failed" -eq 0
test "$line_log_failed" -eq 0
test "$task_transaction_failed" -eq 0
