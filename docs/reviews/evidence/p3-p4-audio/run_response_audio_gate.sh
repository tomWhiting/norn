#!/usr/bin/env bash
set -uo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root" || exit 1

base_commit=460c192
source_commit=$(git rev-parse HEAD)
source_tree=$(git rev-parse HEAD^{tree})
short_commit=$(git rev-parse --short=7 HEAD)
evidence_dir=docs/reviews/evidence/p3-p4-audio
output="$evidence_dir/2026-07-17-response-audio-gate-$short_commit.json"
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
  printf 'response-audio gate source is not frozen:\n%s\n' "$dirty_rust" >&2
  exit 3
fi

gate_results='[]'
gate_failures=0

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

run_gate() {
  local gate_id=$1
  local expected_passed=$2
  shift 2
  local command_text
  printf -v command_text '%q ' "$@"
  command_text=${command_text% }
  local started_at
  started_at=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
  local command_output
  command_output=$("$@" 2>&1)
  local exit_status=$?
  local output_sha256
  output_sha256=$(printf '%s' "$command_output" | shasum -a 256 | awk '{print $1}')
  local observed_passed=0
  if [[ "$expected_passed" != "null" ]]; then
    observed_passed=$(printf '%s\n' "$command_output" | passed_count)
  fi
  local result=pass
  if [[ $exit_status -ne 0 ]]; then
    result=fail
  elif [[ "$expected_passed" != "null" && "$observed_passed" -ne "$expected_passed" ]]; then
    result=fail
  fi
  if [[ "$result" == "fail" ]]; then
    gate_failures=$((gate_failures + 1))
    printf '%s failed (exit=%d observed=%d expected=%s)\n%s\n' \
      "$gate_id" "$exit_status" "$observed_passed" "$expected_passed" \
      "$command_output" >&2
  fi
  local record
  if [[ "$expected_passed" == "null" ]]; then
    record=$(jq -n \
      --arg gate_id "$gate_id" \
      --arg command "$command_text" \
      --arg started_at "$started_at" \
      --arg result "$result" \
      --argjson exit_status "$exit_status" \
      --arg output_sha256 "$output_sha256" \
      '{gate_id: $gate_id, command: $command, started_at: $started_at,
        result: $result, exit_status: $exit_status,
        output_sha256: $output_sha256}')
  else
    record=$(jq -n \
      --arg gate_id "$gate_id" \
      --arg command "$command_text" \
      --arg started_at "$started_at" \
      --arg result "$result" \
      --argjson exit_status "$exit_status" \
      --argjson expected_passed "$expected_passed" \
      --argjson observed_passed "$observed_passed" \
      --arg output_sha256 "$output_sha256" \
      '{gate_id: $gate_id, command: $command, started_at: $started_at,
        result: $result, exit_status: $exit_status,
        expected_passed: $expected_passed, observed_passed: $observed_passed,
        output_sha256: $output_sha256}')
  fi
  gate_results=$(jq --argjson record "$record" '. + [$record]' <<<"$gate_results")
}

run_gate fmt null cargo +1.94.0 fmt --all -- --check
run_gate clippy null cargo +1.94.0 --locked clippy --workspace --all-targets -- -D warnings
run_gate workspace_tests 5345 \
  cargo +1.94.0 --locked test --workspace --all-targets --no-fail-fast
run_gate doctests 8 cargo +1.94.0 --locked test --workspace --doc --no-fail-fast
run_gate response_audio 37 \
  cargo +1.94.0 --locked test -p norn response_audio --lib --no-fail-fast
run_gate publication 24 \
  cargo +1.94.0 --locked test -p norn session::persistence::index::publication:: \
  --lib --no-fail-fast
run_gate filtered_fork 6 \
  cargo +1.94.0 --locked test -p norn fork_canonical_resolution_tests \
  --lib --no-fail-fast
run_gate cli_audio_wire 1 \
  cargo +1.94.0 --locked test -p norn-cli \
  actionable_audio_projection_does_not_duplicate_the_raw_wire_event --no-fail-fast
run_gate tui_audio_redraw 1 \
  cargo +1.94.0 --locked test -p norn-tui \
  actionable_audio_projection_does_not_force_panel_redraw --no-fail-fast

changed_rust=$(git diff --name-only "$base_commit" "$source_commit" -- '*.rs')
changed_rust_count=$(wc -l <<<"$changed_rust" | tr -d ' ')
loc_inventory='[]'
loc_violations='[]'
max_loc=0
max_loc_path=''
while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  production_loc=$(git show "$source_commit:$path" | awk '
    /^#\[cfg\(test\)\]$/ { print NR - 1; found = 1; exit }
    END { if (!found) print NR }
  ')
  entry=$(jq -n --arg path "$path" --argjson production_loc "$production_loc" \
    '{path: $path, production_loc: $production_loc}')
  loc_inventory=$(jq --argjson entry "$entry" '. + [$entry]' <<<"$loc_inventory")
  if ((production_loc > max_loc)); then
    max_loc=$production_loc
    max_loc_path=$path
  fi
  if ((production_loc >= 500)); then
    loc_violations=$(jq --argjson entry "$entry" '. + [$entry]' <<<"$loc_violations")
  fi
done <<<"$changed_rust"

policy_matches=$(
  git diff -U0 "$base_commit" "$source_commit" -- '*.rs' |
    rg '^\+.*(#\[allow|\.unwrap\(|\.unwrap_err\(|\.unwrap_none\(|\.expect\(|\.expect_err\(|panic!|todo!|unimplemented!)' || true
)
policy_match_count=0
if [[ -n "$policy_matches" ]]; then
  policy_match_count=$(wc -l <<<"$policy_matches" | tr -d ' ')
fi

production_prefix_hash() {
  local revision=$1
  local path=$2
  git show "$revision:$path" |
    awk '/^#\[cfg\(test\)\]$/ { exit } { print }' |
    shasum -a 256 | awk '{print $1}'
}

events_base_hash=$(production_prefix_hash "$base_commit" crates/norn/src/session/events.rs)
events_source_hash=$(production_prefix_hash "$source_commit" crates/norn/src/session/events.rs)
reader_base_hash=$(production_prefix_hash "$base_commit" crates/norn/src/session/persistence/strict/reader.rs)
reader_source_hash=$(production_prefix_hash "$source_commit" crates/norn/src/session/persistence/strict/reader.rs)

diagnostics_observed_after_sha=$(file_sha256 "$diagnostics_path")
conventions_observed_after_sha=$(file_sha256 "$conventions_path")
overlay_inputs_same_at_checkpoints=true
if [[ "$diagnostics_observed_before_sha" != "$diagnostics_observed_after_sha" || \
  "$conventions_observed_before_sha" != "$conventions_observed_after_sha" ]]; then
  overlay_inputs_same_at_checkpoints=false
  gate_failures=$((gate_failures + 1))
fi

if [[ $policy_match_count -ne 0 || $(jq 'length' <<<"$loc_violations") -ne 0 ]]; then
  gate_failures=$((gate_failures + 1))
fi
if [[ "$events_base_hash" != "$events_source_hash" || "$reader_base_hash" != "$reader_source_hash" ]]; then
  gate_failures=$((gate_failures + 1))
fi

jq -n \
  --arg schema "norn.response_audio.gate.v3" \
  --arg generated_at "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" \
  --arg base_commit "$(git rev-parse "$base_commit")" \
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
  --argjson gate_failures "$gate_failures" \
  --argjson gates "$gate_results" \
  --argjson changed_rust_count "$changed_rust_count" \
  --argjson policy_match_count "$policy_match_count" \
  --argjson loc_inventory "$loc_inventory" \
  --argjson loc_violations "$loc_violations" \
  --argjson max_loc "$max_loc" \
  --arg max_loc_path "$max_loc_path" \
  --arg events_base_hash "$events_base_hash" \
  --arg events_source_hash "$events_source_hash" \
  --arg reader_base_hash "$reader_base_hash" \
  --arg reader_source_hash "$reader_source_hash" \
  '{schema: $schema, generated_at: $generated_at, base_commit: $base_commit,
    source_commit: $source_commit, source_tree: $source_tree,
    toolchain: $toolchain, target_directory: $target_directory,
    working_tree_disclosure: {
      candidate_boundary: "committed response-audio source plus the disclosed test-only working-tree overlay",
      overlay_inputs_match_at_recorded_checkpoints: $overlay_inputs_same_at_checkpoints,
      overlay_inputs: [
        {path: $diagnostics_path, committed_sha256: $diagnostics_committed_sha,
          observed_before_sha256: $diagnostics_observed_before_sha,
          observed_after_sha256: $diagnostics_observed_after_sha,
          effect: "adds two pre-existing diagnostics tests to the workspace test count"},
        {path: $conventions_path, committed_sha256: $conventions_committed_sha,
          observed_before_sha256: $conventions_observed_before_sha,
          observed_after_sha256: $conventions_observed_after_sha,
          effect: "compiled into those two diagnostics tests through include_str; no response-audio production path reads it"}
      ],
      workspace_test_count_includes_two_tests_from_excluded_path: true
    },
    result: (if $gate_failures == 0 then "pass" else "fail" end),
    gate_failures: $gate_failures, gates: $gates,
    policy: {changed_rust_paths: $changed_rust_count,
      prohibited_added_line_matches: $policy_match_count},
    production_loc: {method: "prefix before first exact #[cfg(test)]",
      maximum: $max_loc, maximum_path: $max_loc_path,
      violations_at_or_above_500: $loc_violations, inventory: $loc_inventory},
    d2_codec: {
      events: {base: $events_base_hash, source: $events_source_hash},
      strict_reader: {base: $reader_base_hash, source: $reader_source_hash}
    }}' >"$partial"
mv "$partial" "$output"
printf '%s\n' "$output"

if [[ $gate_failures -ne 0 ]]; then
  exit 1
fi
