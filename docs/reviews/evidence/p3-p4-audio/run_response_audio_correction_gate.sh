#!/usr/bin/env bash
set -uo pipefail

script_path=${BASH_SOURCE[0]}
script_dir=$(cd -- "$(dirname -- "$script_path")" && pwd)
repo_root=$(git -C "$script_dir" rev-parse --show-toplevel)
cd "$repo_root" || exit 1

source_commit=${1:-}
if [[ -z "$source_commit" ]]; then
  printf 'usage: %s <source-commit>\n' "$0" >&2
  exit 2
fi
source_commit=$(git rev-parse "$source_commit^{commit}")
review_commit=$(git rev-parse '50115bf^{commit}')
source_tree=$(git rev-parse "$source_commit^{tree}")
short_commit=$(git rev-parse --short=7 "$source_commit")
runner_path=docs/reviews/evidence/p3-p4-audio/run_response_audio_correction_gate.sh
runner_commit=$(git log -1 --format=%H -- "$runner_path")
evidence_dir=docs/reviews/evidence/p3-p4-audio
output="$evidence_dir/$(date -u +%F)-response-audio-correction-gate-$short_commit.json"
partial=$output.partial
command_output_path=$evidence_dir/.response-audio-correction-command-output.$$.log

cleanup() {
  rm -f "$partial" "$command_output_path"
}
trap cleanup EXIT

build_target=${CARGO_TARGET_DIR:-"$repo_root/target"}
export CARGO_TARGET_DIR=$build_target

file_sha256() {
  shasum -a 256 "$1" | awk '{print $1}'
}

format_command() {
  local target_quoted
  local command_quoted
  printf -v target_quoted '%q' "$CARGO_TARGET_DIR"
  printf -v command_quoted '%q ' "$@"
  command_quoted=${command_quoted% }
  printf 'CARGO_TARGET_DIR=%s %s' "$target_quoted" "$command_quoted"
}

passed_count() {
  awk '
    /test result: (ok|FAILED)\./ {
      for (field = 1; field <= NF; field++) {
        if ($field == "passed;") {
          total += $(field - 1)
        }
      }
    }
    END { print total + 0 }
  '
}

if ! git merge-base --is-ancestor "$source_commit" HEAD; then
  printf 'source commit is not an ancestor of the runner checkout\n' >&2
  exit 3
fi

dirty_rust=$(
  {
    git diff --name-only HEAD -- '*.rs'
    git ls-files --others --exclude-standard -- '*.rs'
  } | LC_ALL=C sort -u
)
if [[ -n "$dirty_rust" ]]; then
  printf 'response-audio correction source is not frozen:\n%s\n' "$dirty_rust" >&2
  exit 3
fi

later_rust=$(git diff --name-only "$source_commit" HEAD -- '*.rs')
if [[ -n "$later_rust" ]]; then
  printf 'Rust source changed after the correction commit:\n%s\n' "$later_rust" >&2
  exit 3
fi

gate_results='[]'
gate_failures=0

run_gate() {
  local gate_id=$1
  local expected_passed=$2
  shift 2
  local command_text
  command_text=$(format_command "$@")
  local started_at
  started_at=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
  "$@" >"$command_output_path" 2>&1
  local exit_status=$?
  local finished_at
  finished_at=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
  local output_sha256
  output_sha256=$(file_sha256 "$command_output_path")
  local observed_passed=0
  if [[ "$expected_passed" != "null" ]]; then
    observed_passed=$(passed_count <"$command_output_path")
  fi
  local result=pass
  if [[ $exit_status -ne 0 ]]; then
    result=fail
  elif [[ "$expected_passed" != "null" && "$observed_passed" -ne "$expected_passed" ]]; then
    result=fail
  fi
  if [[ "$result" == "fail" ]]; then
    gate_failures=$((gate_failures + 1))
    printf '%s failed (exit=%d observed=%d expected=%s)\n' \
      "$gate_id" "$exit_status" "$observed_passed" "$expected_passed" >&2
    sed -n '1,240p' "$command_output_path" >&2
  fi

  local record
  record=$(jq -n \
    --arg gate_id "$gate_id" \
    --arg command "$command_text" \
    --arg started_at "$started_at" \
    --arg finished_at "$finished_at" \
    --arg result "$result" \
    --argjson exit_status "$exit_status" \
    --arg expected_passed "$expected_passed" \
    --argjson observed_passed "$observed_passed" \
    --arg output_sha256 "$output_sha256" \
    '{gate_id: $gate_id, command: $command, started_at: $started_at,
      finished_at: $finished_at, result: $result, exit_status: $exit_status,
      expected_passed: (if $expected_passed == "null" then null else ($expected_passed | tonumber) end),
      observed_passed: (if $expected_passed == "null" then null else $observed_passed end),
      output_sha256: $output_sha256}')
  gate_results=$(jq --argjson record "$record" '. + [$record]' <<<"$gate_results")
}

run_gate fmt null cargo +1.94.0 fmt --all -- --check
run_gate clippy null cargo +1.94.0 --locked clippy --workspace --all-targets -- -D warnings
run_gate norn_lib 3994 cargo +1.94.0 --locked test -p norn --lib --no-fail-fast
run_gate response_reconciler 113 \
  cargo +1.94.0 --locked test -p norn response_reconciler --lib --no-fail-fast
run_gate response_audio 41 \
  cargo +1.94.0 --locked test -p norn response_audio --lib --no-fail-fast
run_gate m1_fixed_width_and_equality 2 \
  cargo +1.94.0 --locked test -p norn frame_signatures_ --lib --no-fail-fast
run_gate m1_audio_duplicate_semantics 1 \
  cargo +1.94.0 --locked test -p norn \
  audio_delta_exact_duplicate_is_idempotent_but_changed_payload_conflicts \
  --lib --no-fail-fast
run_gate f2_resume_diagnostic 1 \
  cargo +1.94.0 --locked test -p norn \
  resume_preserves_duplicate_audio_artifact_reference_diagnostic \
  --lib --no-fail-fast
run_gate f2_fork_diagnostic 1 \
  cargo +1.94.0 --locked test -p norn \
  fork_preserves_response_audio_link_order_diagnostic --lib --no-fail-fast

changed_rust=$(git diff --name-only "$review_commit" "$source_commit" -- '*.rs')
changed_rust_count=$(printf '%s\n' "$changed_rust" | awk 'NF {count++} END {print count + 0}')
diff_inventory=$(git diff --name-only "$review_commit" "$source_commit" | LC_ALL=C sort)
diff_inventory_count=$(printf '%s\n' "$diff_inventory" | awk 'NF {count++} END {print count + 0}')
diff_inventory_sha256=$(printf '%s\n' "$diff_inventory" | shasum -a 256 | awk '{print $1}')

policy_matches=$(
  git diff -U0 "$review_commit" "$source_commit" -- '*.rs' |
    rg '^\+.*(#\[allow|#\[ignore|\.unwrap\(|\.unwrap_err\(|\.unwrap_none\(|\.expect\(|\.expect_err\(|panic!|todo!|unimplemented!)' || true
)
policy_match_count=0
if [[ -n "$policy_matches" ]]; then
  policy_match_count=$(printf '%s\n' "$policy_matches" | wc -l | tr -d ' ')
fi

loc_inventory='[]'
loc_violations='[]'
while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  production_loc=$(git show "$source_commit:$path" | awk '
    /^#\[cfg\(test\)\]$/ { print NR - 1; found = 1; exit }
    END { if (!found) print NR }
  ')
  entry=$(jq -n --arg path "$path" --argjson production_loc "$production_loc" \
    '{path: $path, production_loc: $production_loc}')
  loc_inventory=$(jq --argjson entry "$entry" '. + [$entry]' <<<"$loc_inventory")
  if ((production_loc >= 500)); then
    loc_violations=$(jq --argjson entry "$entry" '. + [$entry]' <<<"$loc_violations")
  fi
done <<<"$changed_rust"

static_result=pass
if [[ $changed_rust_count -ne 9 || $diff_inventory_count -ne 9 || \
  $policy_match_count -ne 0 || $(jq 'length' <<<"$loc_violations") -ne 0 ]]; then
  static_result=fail
  gate_failures=$((gate_failures + 1))
fi

runner_observed_sha256=$(file_sha256 "$runner_path")

jq -n \
  --arg schema 'norn.response_audio.correction_gate.v1' \
  --arg generated_at "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" \
  --arg review_commit "$review_commit" \
  --arg source_commit "$source_commit" \
  --arg source_tree "$source_tree" \
  --arg runner_commit "$runner_commit" \
  --arg runner_path "$runner_path" \
  --arg runner_observed_sha256 "$runner_observed_sha256" \
  --arg toolchain "$(rustc +1.94.0 --version)" \
  --arg cargo_target_directory "$CARGO_TARGET_DIR" \
  --argjson gate_failures "$gate_failures" \
  --argjson gates "$gate_results" \
  --arg static_result "$static_result" \
  --argjson changed_rust_count "$changed_rust_count" \
  --argjson diff_inventory_count "$diff_inventory_count" \
  --arg diff_inventory_sha256 "$diff_inventory_sha256" \
  --argjson policy_match_count "$policy_match_count" \
  --argjson loc_inventory "$loc_inventory" \
  --argjson loc_violations "$loc_violations" \
  '{schema: $schema, generated_at: $generated_at,
    review: {commit: $review_commit, verdict: "NOT READY", findings: ["M-1", "F-2"]},
    source: {commit: $source_commit, tree: $source_tree, frozen_committed_rust: true},
    runner: {commit: $runner_commit, path: $runner_path,
      observed_sha256: $runner_observed_sha256},
    environment: {toolchain: $toolchain,
      cargo_target_directory: $cargo_target_directory},
    result: (if $gate_failures == 0 then "pass" else "fail" end),
    gate_failures: $gate_failures, gates: $gates,
    correction_scope: {result: $static_result,
      changed_rust_paths: $changed_rust_count,
      complete_diff_paths: $diff_inventory_count,
      complete_diff_inventory_sha256: $diff_inventory_sha256,
      prohibited_added_line_matches: $policy_match_count},
    production_loc: {method: "prefix before first exact #[cfg(test)]",
      violations_at_or_above_500: $loc_violations, inventory: $loc_inventory}}' >"$partial"
mv "$partial" "$output"
printf '%s\n' "$output"

if [[ $gate_failures -ne 0 ]]; then
  exit 1
fi
