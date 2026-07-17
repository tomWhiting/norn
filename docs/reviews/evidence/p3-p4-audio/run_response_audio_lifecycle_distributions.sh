#!/usr/bin/env bash
set -uo pipefail

script_path=${BASH_SOURCE[0]}
script_dir=$(cd -- "$(dirname -- "$script_path")" && pwd)
repo_root=$(git -C "$script_dir" rev-parse --show-toplevel)
cd "$repo_root" || exit 1

repeated_iterations=${1:-20}
if ! [[ "$repeated_iterations" =~ ^[1-9][0-9]*$ ]]; then
  printf 'repeated iterations must be a positive integer\n' >&2
  exit 2
fi
if ((repeated_iterations < 20)); then
  printf 'repeated lifecycle cases require at least 20 iterations\n' >&2
  exit 2
fi

build_target=${CARGO_TARGET_DIR:-"$repo_root/target"}
export CARGO_TARGET_DIR=$build_target

source_commit=$(git rev-parse HEAD)
source_tree=$(git rev-parse HEAD^{tree})
short_commit=$(git rev-parse --short=7 HEAD)
evidence_dir=docs/reviews/evidence/p3-p4-audio
runner_path=$evidence_dir/run_response_audio_lifecycle_distributions.sh
output="$evidence_dir/$(date -u +%F)-response-audio-lifecycle-distributions-$short_commit.json"
partial=$output.partial
command_output_path=$evidence_dir/.response-audio-lifecycle-command-output.$$.log

cleanup() {
  rm -f "$partial" "$command_output_path"
}
trap cleanup EXIT

file_sha256() {
  shasum -a 256 "$1" | awk '{print $1}'
}

revision_sha256() {
  git show "$1:$2" | shasum -a 256 | awk '{print $1}'
}

text_sha256() {
  printf '%s' "$1" | shasum -a 256 | awk '{print $1}'
}

json_sha256() {
  jq -cS . <<<"$1" | shasum -a 256 | awk '{print $1}'
}

format_command() {
  local target_quoted
  local command_quoted
  printf -v target_quoted '%q' "$CARGO_TARGET_DIR"
  printf -v command_quoted '%q ' "$@"
  command_quoted=${command_quoted% }
  printf 'CARGO_TARGET_DIR=%s %s' "$target_quoted" "$command_quoted"
}

dirty_rust=$(
  {
    git diff --name-only HEAD -- '*.rs'
    git ls-files --others --exclude-standard -- '*.rs'
  } | LC_ALL=C sort -u
)
if [[ -n "$dirty_rust" ]]; then
  printf 'response-audio lifecycle evidence source is not frozen:\n%s\n' \
    "$dirty_rust" >&2
  exit 3
fi

rust_source_manifest_sha256=$(
  git ls-tree -r "$source_commit" | awk '$4 ~ /\.rs$/ {print}' |
    shasum -a 256 | awk '{print $1}'
)
runner_observed_sha256=$(file_sha256 "$runner_path")

case_names=(
  absent_response_id_survives_sidecar_link_assistant_and_resume
  completed_audio_step_writes_one_final_terminal_jsonl_record
  invalid_delta_shapes_and_audio_base64_are_typed
  malformed_audio_and_transcript_delta_payloads_fail_typed
  post_llm_hard_cut_preserves_sealed_audio_without_publishing_turn
  top_level_fork_resolves_two_audio_artifacts_after_source_deletion
  fork_under_persistent_parent_persists_child_timeline
)
semantic_case_ids=(
  absent_response_id_lifecycle
  single_final_terminal_record
  malformed_delta_symmetry
  malformed_delta_symmetry
  post_llm_hard_cut
  multi_artifact_ownership_fork
  fork_tool_inherited_audio
)
case_classes=(
  deterministic
  deterministic
  deterministic
  deterministic
  repeated
  repeated
  repeated
)
case_source_paths=(
  crates/norn/src/loop/response_audio_lifecycle_loop_tests.rs
  crates/norn/src/loop/response_audio_lifecycle_loop_tests.rs
  crates/norn/src/provider/response_audio.rs
  crates/norn/src/provider/openai/response_reconciler/tests/audio.rs
  crates/norn/src/loop/response_audio_lifecycle_loop_tests.rs
  crates/norn/src/session/manager/tests/fork_audio.rs
  crates/norn/src/tools/agent/fork_tool.rs
)

semantic_case_count=$(
  printf '%s\n' "${semantic_case_ids[@]}" | LC_ALL=C sort -u | wc -l | tr -d ' '
)
test_invocation_count=${#case_names[@]}

case_results='[]'
total_passed=0
total_failed=0

for index in "${!case_names[@]}"; do
  test_name=${case_names[$index]}
  semantic_case_id=${semantic_case_ids[$index]}
  case_class=${case_classes[$index]}
  source_path=${case_source_paths[$index]}
  iterations=1
  if [[ "$case_class" == repeated ]]; then
    iterations=$repeated_iterations
  fi

  command=(
    cargo +1.94.0 --locked test -p norn "$test_name" --lib --no-fail-fast
  )
  command_text=$(format_command "${command[@]}")
  command_sha256=$(text_sha256 "$command_text")
  source_sha256=$(revision_sha256 "$source_commit" "$source_path")
  observations='[]'
  passed=0
  failed=0

  for ((iteration = 1; iteration <= iterations; iteration++)); do
    started_at=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
    "${command[@]}" >"$command_output_path" 2>&1
    exit_status=$?
    finished_at=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
    output_sha256=$(file_sha256 "$command_output_path")
    result=fail
    if [[ $exit_status -eq 0 ]] &&
      grep -q 'test result: ok. 1 passed; 0 failed' "$command_output_path"; then
      result=pass
      passed=$((passed + 1))
      total_passed=$((total_passed + 1))
    else
      failed=$((failed + 1))
      total_failed=$((total_failed + 1))
      printf '%s iteration %d failed (exit %d)\n' \
        "$test_name" "$iteration" "$exit_status" >&2
      sed -n '1,240p' "$command_output_path" >&2
    fi

    observation=$(jq -n \
      --argjson iteration "$iteration" \
      --arg started_at "$started_at" \
      --arg finished_at "$finished_at" \
      --arg result "$result" \
      --argjson exit_status "$exit_status" \
      --arg output_sha256 "$output_sha256" \
      '{iteration: $iteration, started_at: $started_at,
        finished_at: $finished_at, result: $result,
        exit_status: $exit_status, output_sha256: $output_sha256}')
    observations=$(jq --argjson observation "$observation" \
      '. + [$observation]' <<<"$observations")
  done

  observations_sha256=$(json_sha256 "$observations")
  case_result=$(jq -n \
    --arg test_name "$test_name" \
    --arg semantic_case_id "$semantic_case_id" \
    --arg case_class "$case_class" \
    --arg source_path "$source_path" \
    --arg source_sha256 "$source_sha256" \
    --arg command "$command_text" \
    --arg command_sha256 "$command_sha256" \
    --argjson iterations "$iterations" \
    --argjson passed "$passed" \
    --argjson failed "$failed" \
    --arg observations_sha256 "$observations_sha256" \
    --argjson observations "$observations" \
    '{semantic_case_id: $semantic_case_id, test_name: $test_name,
      class: $case_class,
      source: {path: $source_path, sha256: $source_sha256},
      command: $command, command_sha256: $command_sha256,
      iterations: $iterations, passed: $passed, failed: $failed,
      observations_sha256: $observations_sha256,
      observations: $observations}')
  case_results=$(jq --argjson case_result "$case_result" \
    '. + [$case_result]' <<<"$case_results")
done

jq -n \
  --arg schema 'norn.response_audio.lifecycle_distributions.v1' \
  --arg generated_at "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" \
  --arg source_commit "$source_commit" \
  --arg source_tree "$source_tree" \
  --arg rust_source_manifest_sha256 "$rust_source_manifest_sha256" \
  --arg runner_path "$runner_path" \
  --arg runner_observed_sha256 "$runner_observed_sha256" \
  --arg toolchain "$(rustc +1.94.0 --version)" \
  --arg cargo_target_directory "$CARGO_TARGET_DIR" \
  --argjson repeated_minimum 20 \
  --argjson repeated_iterations "$repeated_iterations" \
  --argjson semantic_case_count "$semantic_case_count" \
  --argjson test_invocation_count "$test_invocation_count" \
  --argjson total_passed "$total_passed" \
  --argjson total_failed "$total_failed" \
  --argjson cases "$case_results" \
  '{schema: $schema, generated_at: $generated_at,
    source: {commit: $source_commit, tree: $source_tree,
      rust_manifest_sha256: $rust_source_manifest_sha256,
      frozen_committed_rust: true},
    runner: {path: $runner_path, observed_sha256: $runner_observed_sha256},
    environment: {toolchain: $toolchain,
      cargo_target_directory: $cargo_target_directory},
    distribution_policy: {repeated_case_minimum: $repeated_minimum,
      repeated_case_iterations: $repeated_iterations,
      deterministic_case_iterations: 1},
    coverage: {semantic_cases: $semantic_case_count,
      test_invocations: $test_invocation_count},
    result: (if $total_failed == 0 then "pass" else "fail" end),
    totals: {passed: $total_passed, failed: $total_failed},
    cases: $cases}' >"$partial"
mv "$partial" "$output"
printf '%s\n' "$output"

if [[ $total_failed -ne 0 ]]; then
  exit 1
fi
