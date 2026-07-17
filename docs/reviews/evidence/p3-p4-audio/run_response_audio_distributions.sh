#!/usr/bin/env bash
set -uo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root" || exit 1

iterations=${1:-20}
if ! [[ "$iterations" =~ ^[1-9][0-9]*$ ]]; then
  printf 'iterations must be a positive integer\n' >&2
  exit 2
fi

source_commit=$(git rev-parse HEAD)
source_tree=$(git rev-parse HEAD^{tree})
short_commit=$(git rev-parse --short=7 HEAD)
evidence_dir=docs/reviews/evidence/p3-p4-audio
output="$evidence_dir/2026-07-17-response-audio-distributions-$short_commit.json"
partial="$output.partial"
diagnostics_path=crates/norn/src/tools/diagnostics_check/tests.rs
conventions_path=CONVENTIONS.toml

revision_sha256() {
  local revision=$1
  local path=$2
  git show "$revision:$path" | shasum -a 256 | awk '{print $1}'
}

file_sha256() {
  local path=$1
  shasum -a 256 "$path" | awk '{print $1}'
}

diagnostics_committed_sha=$(revision_sha256 "$source_commit" "$diagnostics_path")
diagnostics_observed_before_sha=$(file_sha256 "$diagnostics_path")
conventions_committed_sha=$(revision_sha256 "$source_commit" "$conventions_path")
conventions_observed_before_sha=$(file_sha256 "$conventions_path")

dirty_rust=$(
  {
    git diff --name-only HEAD -- '*.rs'
    git ls-files --others --exclude-standard -- '*.rs'
  } | grep -v "^${diagnostics_path}$" || true
)
if [[ -n "$dirty_rust" ]]; then
  printf 'response-audio evidence source is not frozen:\n%s\n' "$dirty_rust" >&2
  exit 3
fi

cases=(
  cancellation_after_audio_frame_persists_only_unsealed_partial_reference
  every_durable_audio_publication_checkpoint_recovers_without_a_dangling_reference
  top_level_fork_owns_copied_response_audio_after_source_deletion
  session_event_hook_may_append_between_audio_link_and_assistant
  audio_fork_deduplicates_references_and_survives_source_artifact_deletion
)

case_results='[]'
total_passed=0
total_failed=0

for test_name in "${cases[@]}"; do
  observations='[]'
  passed=0
  failed=0
  for ((iteration = 1; iteration <= iterations; iteration++)); do
    started_at=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
    command=(
      cargo +1.94.0 --locked test -p norn "$test_name" --lib --no-fail-fast
    )
    command_output=$("${command[@]}" 2>&1)
    exit_status=$?
    output_sha256=$(printf '%s' "$command_output" | shasum -a 256 | awk '{print $1}')
    if [[ $exit_status -eq 0 ]] && grep -q 'test result: ok. 1 passed; 0 failed' <<<"$command_output"; then
      result=pass
      passed=$((passed + 1))
      total_passed=$((total_passed + 1))
    else
      result=fail
      failed=$((failed + 1))
      total_failed=$((total_failed + 1))
      printf '%s iteration %d failed (exit %d)\n%s\n' \
        "$test_name" "$iteration" "$exit_status" "$command_output" >&2
    fi
    observation=$(jq -n \
      --argjson iteration "$iteration" \
      --arg started_at "$started_at" \
      --arg result "$result" \
      --argjson exit_status "$exit_status" \
      --arg output_sha256 "$output_sha256" \
      '{iteration: $iteration, started_at: $started_at, result: $result,
        exit_status: $exit_status, output_sha256: $output_sha256}')
    observations=$(jq --argjson observation "$observation" \
      '. + [$observation]' <<<"$observations")
  done
  case_result=$(jq -n \
    --arg test_name "$test_name" \
    --argjson iterations "$iterations" \
    --argjson passed "$passed" \
    --argjson failed "$failed" \
    --argjson observations "$observations" \
    '{test_name: $test_name, iterations: $iterations, passed: $passed,
      failed: $failed, observations: $observations}')
  case_results=$(jq --argjson case_result "$case_result" \
    '. + [$case_result]' <<<"$case_results")
done

diagnostics_observed_after_sha=$(file_sha256 "$diagnostics_path")
conventions_observed_after_sha=$(file_sha256 "$conventions_path")
overlay_inputs_same_at_checkpoints=true
if [[ "$diagnostics_observed_before_sha" != "$diagnostics_observed_after_sha" || \
  "$conventions_observed_before_sha" != "$conventions_observed_after_sha" ]]; then
  overlay_inputs_same_at_checkpoints=false
  total_failed=$((total_failed + 1))
fi

jq -n \
  --arg schema "norn.response_audio.distributions.v3" \
  --arg generated_at "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" \
  --arg source_commit "$source_commit" \
  --arg source_tree "$source_tree" \
  --arg toolchain "$(rustc +1.94.0 --version)" \
  --arg target_directory "target" \
  --arg diagnostics_path "$diagnostics_path" \
  --arg diagnostics_committed_sha "$diagnostics_committed_sha" \
  --arg diagnostics_observed_before_sha "$diagnostics_observed_before_sha" \
  --arg diagnostics_observed_after_sha "$diagnostics_observed_after_sha" \
  --arg conventions_path "$conventions_path" \
  --arg conventions_committed_sha "$conventions_committed_sha" \
  --arg conventions_observed_before_sha "$conventions_observed_before_sha" \
  --arg conventions_observed_after_sha "$conventions_observed_after_sha" \
  --argjson overlay_inputs_same_at_checkpoints "$overlay_inputs_same_at_checkpoints" \
  --argjson cases "$case_results" \
  --argjson total_passed "$total_passed" \
  --argjson total_failed "$total_failed" \
  '{schema: $schema, generated_at: $generated_at, source_commit: $source_commit,
    source_tree: $source_tree, toolchain: $toolchain,
    target_directory: $target_directory,
    working_tree_disclosure: {
      candidate_boundary: "committed response-audio source plus the disclosed test-only working-tree overlay",
      overlay_inputs_match_at_recorded_checkpoints: $overlay_inputs_same_at_checkpoints,
      overlay_inputs: [
        {path: $diagnostics_path, committed_sha256: $diagnostics_committed_sha,
          observed_before_sha256: $diagnostics_observed_before_sha,
          observed_after_sha256: $diagnostics_observed_after_sha,
          effect: "compiled into the test binary but none of the five repeated response-audio tests executes this module"},
        {path: $conventions_path, committed_sha256: $conventions_committed_sha,
          observed_before_sha256: $conventions_observed_before_sha,
          observed_after_sha256: $conventions_observed_after_sha,
          effect: "compiled into the diagnostics test module through include_str; no repeated response-audio test reads it"}
      ]
    },
    totals: {passed: $total_passed, failed: $total_failed}, cases: $cases}' \
  >"$partial"
mv "$partial" "$output"
printf '%s\n' "$output"

if [[ $total_failed -ne 0 ]]; then
  exit 1
fi
