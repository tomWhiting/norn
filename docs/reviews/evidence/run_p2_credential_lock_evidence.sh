#!/bin/sh
set -eu

iterations="${1:-20}"
output="${2:-docs/reviews/evidence/2026-07-15-p2-credential-lock.json}"
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
if [ "${CARGO_TARGET_DIR+x}" = x ]; then
    echo "credential-lock evidence must use the repository's normal target directory" >&2
    exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "credential-lock evidence requires jq to verify Cargo's target directory" >&2
    exit 2
fi
repository_root="$(git rev-parse --show-toplevel)"
target_directory="$(
    cargo metadata --locked --no-deps --format-version 1 | jq -r '.target_directory'
)"
if [ "$target_directory" != "$repository_root/target" ]; then
    echo "credential-lock evidence must use the repository's normal target directory" >&2
    exit 2
fi
if ! git diff --quiet -- crates/norn/src/provider/auth.rs crates/norn/src/provider/openai_oauth; then
    echo "credential-lock evidence requires committed OAuth source" >&2
    exit 2
fi
if ! git diff --cached --quiet -- crates/norn/src/provider/auth.rs crates/norn/src/provider/openai_oauth; then
    echo "credential-lock evidence requires committed OAuth source" >&2
    exit 2
fi
untracked_source="$(
    git ls-files --others --exclude-standard -- \
        crates/norn/src/provider/auth.rs crates/norn/src/provider/openai_oauth
)"
if [ -n "$untracked_source" ]; then
    echo "credential-lock evidence requires committed OAuth source" >&2
    exit 2
fi

head="$(git rev-parse HEAD)"
platform="$(uname -srm)"
rustc_version="$(rustc --version)"
cargo_version="$(cargo --version)"
log="target/evidence/p2-credential-lock.log"
mkdir -p "$(dirname "$log")"
filters='provider::openai_oauth::credential_transaction::tests::process_gate_honors_the_caller_deadline
provider::openai_oauth::manager::process_tests::two_process_refresh_converges_with_one_authority_exchange'

results=''
for filter in $filters; do
    passed=0
    failed=0
    iteration=1
    while [ "$iteration" -le "$iterations" ]; do
        if CARGO_BUILD_JOBS=1 cargo test --locked -p norn --lib "$filter" -- --exact \
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
printf '{\n  "head": "%s",\n  "oauth_source_committed": true,\n  "normal_target": true,\n  "target_directory": "target/",\n  "platform": "%s",\n  "rustc": "%s",\n  "cargo": "%s",\n  "iterations": %s,\n  "cases": [\n%s\n  ]\n}\n' \
    "$head" "$platform" "$rustc_version" "$cargo_version" "$iterations" "$results" \
    >"$output"
cat "$output"
if grep -q '"failed": [^0]' "$output"; then
    exit 1
fi
