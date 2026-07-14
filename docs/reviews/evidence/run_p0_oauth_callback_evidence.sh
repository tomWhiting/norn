#!/bin/sh
set -eu

iterations="${1:-20}"
output="${2:-docs/reviews/evidence/2026-07-14-p0-oauth-callback.json}"
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
if [ -n "$(git status --porcelain)" ]; then
    echo "OAuth callback evidence requires a clean worktree" >&2
    exit 2
fi

head="$(git rev-parse HEAD)"
platform="$(uname -srm)"
rustc_version="$(rustc --version)"
cargo_version="$(cargo --version)"
log="$(mktemp)"
trap 'rm -f "$log"' EXIT HUP INT TERM
filters='provider::openai_oauth::login_server::tests::accepted_connection_waits_for_delayed_request_bytes
provider::openai_oauth::login_server::tests::matching_error_callback_fails_the_flow_with_a_400_page
provider::openai_oauth::login_server::tests::stray_requests_get_404_and_login_still_completes
provider::openai_oauth::login_server::tests::wait_times_out_when_no_matching_callback_arrives'

results=''
for filter in $filters; do
    passed=0
    failed=0
    iteration=1
    while [ "$iteration" -le "$iterations" ]; do
        if CARGO_INCREMENTAL=0 cargo test -p norn --lib "$filter" -- --exact \
            >"$log" 2>&1; then
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
printf '{\n  "head": "%s",\n  "worktree_clean": true,\n  "platform": "%s",\n  "rustc": "%s",\n  "cargo": "%s",\n  "iterations": %s,\n  "cases": [\n%s\n  ]\n}\n' \
    "$head" "$platform" "$rustc_version" "$cargo_version" "$iterations" "$results" \
    >"$output"
cat "$output"
if grep -q '"failed": [^0]' "$output"; then
    exit 1
fi
