#!/usr/bin/env bash
set -uo pipefail

script_path=${BASH_SOURCE[0]}
script_dir=$(cd -- "$(dirname -- "$script_path")" && pwd)
repo_root=$(git -C "$script_dir" rev-parse --show-toplevel)
cd "$repo_root" || exit 1

source_arg=${1:-}
if [[ -z "$source_arg" ]]; then
  printf 'usage: %s <source-commit>\n' "$0" >&2
  exit 2
fi
if ! source_commit=$(git rev-parse "$source_arg^{commit}"); then
  exit 2
fi
base_commit=$(git rev-parse '624540d^{commit}')
source_tree=$(git rev-parse "$source_commit^{tree}")
short_commit=$(git rev-parse --short=7 "$source_commit")

runner_path=docs/reviews/evidence/p3-p4/run_response_optional_lifecycle_gate.sh
contract_path=docs/reviews/evidence/p3-p4/2026-07-18-response-optional-shape-contract.json
inventory_path=docs/reviews/evidence/p3-p4/2026-07-18-response-lifecycle-surface-inventory.json
contract_expected_sha256=7ea54c502732510a8c7e55a2acbfd481b92ed267ded34ce747af79216883dea3
inventory_expected_sha256=561f9cc099d31a9d6c0e7ac40f28cd477db8058e7d71a7b400ccabe7f3ad11f0
inventory_expected_surface_count=10
inventory_expected_anchor_count=63
inventory_expected_unique_test_count=44
contract_source_section_sha256=d414cb294fadb4b56185f6507fe57a092dfb10e888776b24aa866be89d3182ea
evidence_dir=docs/reviews/evidence/p3-p4
output="$evidence_dir/$(date -u +%F)-response-optional-lifecycle-gate-$short_commit.json"

repo_target="$repo_root/target"
mkdir -p "$repo_target"
build_target=${CARGO_TARGET_DIR:-$repo_target}
if [[ "$build_target" != /* ]]; then
  build_target="$repo_root/$build_target"
fi
mkdir -p "$build_target"
repo_target=$(cd "$repo_target" && pwd -P)
build_target=$(cd "$build_target" && pwd -P)
os_tmp=$(cd "${TMPDIR:-/tmp}" && pwd -P)
case "$build_target" in
  "$repo_target" | "$repo_target"/*) ;;
  *)
    printf 'CARGO_TARGET_DIR must be the repository target directory or its child\n' >&2
    exit 2
    ;;
esac
case "$build_target" in
  "$os_tmp" | "$os_tmp"/* | /tmp | /tmp/* | /private/tmp | /private/tmp/*)
    printf 'CARGO_TARGET_DIR must not resolve inside an OS temporary directory\n' >&2
    exit 2
    ;;
esac
export CARGO_TARGET_DIR=$build_target

scratch="$CARGO_TARGET_DIR/response-optional-lifecycle-gate-$short_commit-$$"
mkdir -p "$scratch"
contract_copy=$scratch/optional-contract.json
inventory_copy=$scratch/lifecycle-inventory.json
partial=$scratch/result.json
command_log=$scratch/command.log

cleanup() {
  rm -rf -- "$scratch"
}
trap cleanup EXIT

file_sha256() {
  shasum -a 256 "$1" | awk '{print $1}'
}

stream_sha256() {
  shasum -a 256 | awk '{print $1}'
}

passed_count() {
  awk '
    /test result: (ok|FAILED)\./ {
      for (field = 1; field <= NF; field++) {
        if ($field == "passed;") total += $(field - 1)
      }
    }
    END { print total + 0 }
  '
}

format_command() {
  local target_quoted command_quoted
  printf -v target_quoted '%q' "$CARGO_TARGET_DIR"
  printf -v command_quoted '%q ' "$@"
  command_quoted=${command_quoted% }
  printf 'CARGO_TARGET_DIR=%s %s' "$target_quoted" "$command_quoted"
}

if ! git merge-base --is-ancestor "$base_commit" "$source_commit"; then
  printf 'base commit is not an ancestor of source commit\n' >&2
  exit 3
fi
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
later_rust=$(git diff --name-only "$source_commit" HEAD -- '*.rs')
if [[ -n "$dirty_rust" || -n "$later_rust" ]]; then
  printf 'Rust source is not frozen at %s\n' "$source_commit" >&2
  exit 3
fi
for path in "$runner_path" "$contract_path" "$inventory_path"; do
  if ! git cat-file -e "$source_commit:$path"; then
    printf 'source commit does not contain %s\n' "$path" >&2
    exit 3
  fi
done
runner_source_sha256=$(git show "$source_commit:$runner_path" | stream_sha256)
if [[ $(file_sha256 "$runner_path") != "$runner_source_sha256" ]]; then
  printf 'working runner differs from source-bound runner\n' >&2
  exit 3
fi
git show "$source_commit:$contract_path" >"$contract_copy"
git show "$source_commit:$inventory_path" >"$inventory_copy"

gate_failures=0
checks='[]'

run_check() {
  local check_id=$1 test_output=$2
  shift 2
  local command_text started_at finished_at exit_status observed_passed result output_sha256 record
  command_text=$(format_command "$@")
  started_at=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
  "$@" >"$command_log" 2>&1
  exit_status=$?
  finished_at=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
  observed_passed=null
  if [[ "$test_output" == test ]]; then
    observed_passed=$(passed_count <"$command_log")
  fi
  result=pass
  if [[ $exit_status -ne 0 || "$observed_passed" == 0 ]]; then
    result=fail
    gate_failures=$((gate_failures + 1))
    printf '%s failed\n' "$check_id" >&2
    sed -n '1,240p' "$command_log" >&2
  fi
  output_sha256=$(file_sha256 "$command_log")
  record=$(jq -n --arg id "$check_id" --arg command "$command_text" \
    --arg started_at "$started_at" --arg finished_at "$finished_at" \
    --arg result "$result" --argjson exit_status "$exit_status" \
    --arg observed_passed "$observed_passed" --arg output_sha256 "$output_sha256" \
    '{id: $id, command: $command, started_at: $started_at, finished_at: $finished_at,
      result: $result, exit_status: $exit_status,
      observed_passed: (if $observed_passed == "null" then null else ($observed_passed | tonumber) end),
      output_sha256: $output_sha256}')
  checks=$(jq --argjson record "$record" '. + [$record]' <<<"$checks")
}

verify_contract() {
  [[ $(file_sha256 "$contract_copy") == "$contract_expected_sha256" ]] || return 1
  jq -e --arg section_sha "$contract_source_section_sha256" '
    .source.openapi_version == "2.3.0" and
    .source.section_sha256 == $section_sha and
    .source.section_character_length == 2103673 and
    .source.section_utf8_byte_length == 2103677 and
    .totals.output_items == 28 and
    .totals.contextual_property_occurrences == 274 and
    .totals.state_assertions == 659 and
    .totals.categories == {
      optional_non_nullable: 149,
      optional_nullable: 111,
      required_nullable: 14
    } and
    (.per_item | length) == 28 and
    ([.per_item[].item] | unique | length) == 28 and
    (.properties | length) == 274 and
    ([.properties[] | .legal_states | length] | add) == 659 and
    ([.properties[] | .item + "\u0000" + .contextual_path] | unique | length) == 274 and
    ([.properties[] | select(.category == "optional_non_nullable")] | length) == 149 and
    ([.properties[] | select(.category == "optional_nullable")] | length) == 111 and
    ([.properties[] | select(.category == "required_nullable")] | length) == 14 and
    ([.per_item[].property_occurrences] | add) == 274 and
    ([.per_item[].state_assertions] | add) == 659 and
    all(.properties[];
      (.oas_ref | type) == "string" and
      (.oas_ref | startswith("#/components/schemas/"))) and
    all(.properties[];
      if .category == "optional_non_nullable" then .legal_states == ["absent", "present"]
      elif .category == "optional_nullable" then .legal_states == ["absent", "null", "present"]
      elif .category == "required_nullable" then .legal_states == ["null", "present"]
      else false end)
  ' "$contract_copy"
}

surface_count=0
anchor_count=0
unique_anchor_count=0
resolved_anchor_count=0
verify_inventory() {
  local expected_classes expected_surfaces
  expected_surfaces='["AuthoritativeSchema","LiveReconcile","StrictPersistReload","StoreFalseReplay","PersistentSpawn","LibraryContextFilter","PersistentInRootFork","OwnershipChangingTopLevelFork","ResponseAudioSidecar","FailureBoundary"]'
  expected_classes='["populated_public_success","minimal_public_success","nested_union_success","opaque_historical","wire_failure","event_common_envelope","response_audio_sidecar"]'
  if [[ "$inventory_expected_sha256" == PENDING_* || \
    $inventory_expected_surface_count -eq 0 || \
    $inventory_expected_anchor_count -eq 0 || \
    $inventory_expected_unique_test_count -eq 0 ]]; then
    printf 'lifecycle inventory SHA/count pins are still pending\n' >&2
    return 1
  fi
  [[ $(file_sha256 "$inventory_copy") == "$inventory_expected_sha256" ]] || return 1
  surface_count=$(jq '.surfaces | length' "$inventory_copy")
  anchor_count=$(jq '[.surfaces[].anchor_ids[]] | length' "$inventory_copy")
  unique_anchor_count=$(jq \
    '[. as $root | $root.surfaces[].anchor_ids[] as $id |
      $root.anchor_catalog[$id].test] | unique | length' "$inventory_copy")
  resolved_anchor_count=0
  while IFS=$'\t' read -r path test_name; do
    git cat-file -e "$source_commit:$path" || return 1
    git grep -Fq "fn $test_name" "$source_commit" -- "$path" || return 1
    resolved_anchor_count=$((resolved_anchor_count + 1))
  done < <(jq -r \
    '. as $root | $root.surfaces[].anchor_ids[] as $id |
      $root.anchor_catalog[$id] | [.path, .test] | @tsv' "$inventory_copy")
  [[ $surface_count -eq $inventory_expected_surface_count && \
    $anchor_count -eq $inventory_expected_anchor_count && \
    $unique_anchor_count -eq $inventory_expected_unique_test_count && \
    $resolved_anchor_count -eq $inventory_expected_anchor_count ]] || return 1
  jq -e --arg hash "$contract_expected_sha256" \
    --argjson expected_surfaces "$expected_surfaces" \
    --argjson expected_classes "$expected_classes" '
      . as $root |
      ($root.anchor_catalog | keys) as $catalog_ids |
      ([$root.surfaces[].anchor_ids[]] | unique) as $surface_anchor_ids |
      ([$root.applicability_matrix.classes[].applicability[].anchor_ids[]?] | unique)
        as $matrix_anchor_ids |
      ($root.surfaces | map(.surface)) as $actual_surfaces |
      ($root.applicability_matrix.classes | map(.class)) as $actual_classes |
      $root.coverage_semantics.official_contract_enumeration.artifact_sha256 == $hash and
      $root.coverage_semantics.official_contract_enumeration.output_item_variants == 28 and
      $root.coverage_semantics.official_contract_enumeration.contextual_property_occurrences == 274 and
      $root.coverage_semantics.official_contract_enumeration.legal_state_assertions == 659 and
      $actual_surfaces == $expected_surfaces and
      $actual_classes == $expected_classes and
      $root.applicability_matrix.surface_order == $expected_surfaces and
      $root.applicability_matrix.surface_count == 10 and
      $root.applicability_matrix.class_count == 7 and
      $root.applicability_matrix.cell_count == 70 and
      ([ $root.applicability_matrix.classes[].applicability[] ] | length) == 70 and
      (($catalog_ids - $surface_anchor_ids) | length) == 0 and
      (($surface_anchor_ids - $catalog_ids) | length) == 0 and
      (($matrix_anchor_ids - $catalog_ids) | length) == 0 and
      all($root.anchor_catalog[];
        (.path | type) == "string" and (.path | length) > 0 and
        (.test | type) == "string" and (.test | length) > 0) and
      all($root.surfaces[];
        (.anchor_ids | type) == "array" and (.anchor_ids | length) > 0 and
        ((.anchor_ids | unique | length) == (.anchor_ids | length))) and
      all($root.applicability_matrix.classes[];
        (.applicability | length) == 10 and
        ([.applicability[].surface] == $expected_surfaces) and
        all(.applicability[];
          . as $cell |
          if $cell.status == "covered" then
            ($cell.anchor_ids | type) == "array" and
            ($cell.anchor_ids | length) > 0 and
            (($cell.anchor_ids | unique | length) == ($cell.anchor_ids | length)) and
            (($root.surfaces[] | select(.surface == $cell.surface) | .anchor_ids) as $allowed |
              any($cell.anchor_ids[]; . as $id | ($allowed | index($id)) != null))
          elif $cell.status == "not_applicable" then
            ($cell.reason | type) == "string" and ($cell.reason | length) > 0 and
            (($cell.anchor_ids // []) | length) == 0
          else false end))
    ' "$inventory_copy" >/dev/null
}

policy_matches_json='[]'
loc_inventory='[]'
loc_violations='[]'
verify_diff_policy() {
  local base_prefix_sha base_production_loc entry path policy_matches
  local production_loc production_prefix_unchanged source_prefix_sha source_sha
  policy_matches=$(
    git diff -U0 "$base_commit" "$source_commit" -- '*.rs' |
      rg '^\+[^+].*(#\[[^]]*(allow|expect|ignore)[[:space:]]*\(|\.unwrap(_err|_none)?\(|\.expect(_err)?\(|panic[[:space:]]*!|TODO|todo[[:space:]]*!|unimplemented[[:space:]]*!)' || true
  )
  policy_matches_json=$(printf '%s\n' "$policy_matches" | jq -Rsc 'split("\n") | map(select(length > 0))')
  loc_inventory='[]'
  loc_violations='[]'
  while IFS= read -r -d '' path; do
    production_loc=$(git show "$source_commit:$path" | awk '
      /^#\[cfg\(test\)\]$/ { print NR - 1; found = 1; exit }
      END { if (!found) print NR }
    ')
    source_sha=$(git show "$source_commit:$path" | stream_sha256)
    source_prefix_sha=$(git show "$source_commit:$path" |
      sed -n "1,${production_loc}p" | stream_sha256)
    base_production_loc=null
    base_prefix_sha=
    production_prefix_unchanged=false
    if git cat-file -e "$base_commit:$path" 2>/dev/null; then
      base_production_loc=$(git show "$base_commit:$path" | awk '
        /^#\[cfg\(test\)\]$/ { print NR - 1; found = 1; exit }
        END { if (!found) print NR }
      ')
      base_prefix_sha=$(git show "$base_commit:$path" |
        sed -n "1,${base_production_loc}p" | stream_sha256)
      if [[ $source_prefix_sha == "$base_prefix_sha" ]]; then
        production_prefix_unchanged=true
      fi
    fi
    entry=$(jq -n --arg path "$path" --arg sha256 "$source_sha" \
      --arg source_prefix_sha256 "$source_prefix_sha" \
      --arg base_prefix_sha256 "$base_prefix_sha" \
      --argjson production_loc "$production_loc" \
      --argjson base_production_loc "$base_production_loc" \
      --argjson production_prefix_unchanged "$production_prefix_unchanged" \
      '{path: $path, sha256: $sha256, production_loc: $production_loc,
        production_prefix_sha256: $source_prefix_sha256,
        base_production_loc: $base_production_loc,
        base_production_prefix_sha256:
          (if $base_prefix_sha256 == "" then null else $base_prefix_sha256 end),
        production_prefix_unchanged: $production_prefix_unchanged}')
    loc_inventory=$(jq --argjson entry "$entry" '. + [$entry]' <<<"$loc_inventory")
    if ((production_loc >= 500)) && [[ $production_prefix_unchanged != true ]]; then
      loc_violations=$(jq --argjson entry "$entry" '. + [$entry]' <<<"$loc_violations")
    fi
  done < <(git diff --name-only --diff-filter=ACMR -z "$base_commit" "$source_commit" -- '*.rs')
  [[ $(jq 'length' <<<"$policy_matches_json") -eq 0 && \
    $(jq 'length' <<<"$loc_violations") -eq 0 ]]
}

run_check fmt static cargo +1.94.0 fmt --all -- --check
run_check diff_check static git diff --check "$base_commit" "$source_commit"
run_check clippy static cargo +1.94.0 --locked clippy --workspace --all-targets --all-features -- -D warnings
run_check norn_lib test cargo +1.94.0 --locked test -p norn --lib --all-features --no-fail-fast
run_check optional_contract static verify_contract
run_check lifecycle_inventory static verify_inventory
run_check diff_policy static verify_diff_policy

focused_results='[]'
focused_passed=0
focused_failed=0
while IFS= read -r test_name; do
  iterations=1
  case "$test_name" in
    unknown_item_survives_strict_reload_into_stateless_replay | \
      manager_fork_preserves_unknown_item_under_new_owner_and_strict_resume | \
      spawn_under_persistent_parent_persists_child_timeline | \
      fork_under_persistent_parent_persists_child_timeline)
      iterations=20
      ;;
  esac
  observations='[]'
  passed=0
  failed=0
  command=(cargo +1.94.0 --locked test -p norn --lib --all-features "$test_name" -- --nocapture)
  command_text=$(format_command "${command[@]}")
  for ((iteration = 1; iteration <= iterations; iteration++)); do
    started_at=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
    "${command[@]}" >"$command_log" 2>&1
    exit_status=$?
    finished_at=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
    observed_passed=$(passed_count <"$command_log")
    result=fail
    if [[ $exit_status -eq 0 && $observed_passed -eq 1 ]]; then
      result=pass
      passed=$((passed + 1))
      focused_passed=$((focused_passed + 1))
    else
      failed=$((failed + 1))
      focused_failed=$((focused_failed + 1))
      gate_failures=$((gate_failures + 1))
      printf '%s iteration %d failed or was ambiguous\n' "$test_name" "$iteration" >&2
      sed -n '1,240p' "$command_log" >&2
    fi
    observation=$(jq -n --argjson iteration "$iteration" --arg result "$result" \
      --argjson exit_status "$exit_status" --argjson observed_passed "$observed_passed" \
      --arg started_at "$started_at" --arg finished_at "$finished_at" \
      --arg output_sha256 "$(file_sha256 "$command_log")" \
      '{iteration: $iteration, result: $result, exit_status: $exit_status,
        observed_passed: $observed_passed, started_at: $started_at,
        finished_at: $finished_at, output_sha256: $output_sha256}')
    observations=$(jq --argjson observation "$observation" '. + [$observation]' <<<"$observations")
  done
  anchors=$(jq -c --arg test "$test_name" '
    [. as $root | $root.surfaces[] as $surface | $surface.anchor_ids[] as $id |
      select($root.anchor_catalog[$id].test == $test) |
      {surface: $surface.surface, path: $root.anchor_catalog[$id].path}]
    ' "$inventory_copy")
  test_result=$(jq -n --arg test "$test_name" --arg command "$command_text" \
    --argjson iterations "$iterations" --argjson passed "$passed" --argjson failed "$failed" \
    --argjson anchors "$anchors" --argjson observations "$observations" \
    '{test: $test, anchors: $anchors, command: $command, iterations: $iterations,
      passed: $passed, failed: $failed, observations: $observations}')
  focused_results=$(jq --argjson result "$test_result" '. + [$result]' <<<"$focused_results")
done < <(jq -r '
  [. as $root | $root.surfaces[].anchor_ids[] as $id | $root.anchor_catalog[$id].test] |
  unique[]
  ' "$inventory_copy")

rust_manifest_sha256=$(git ls-tree -r "$source_commit" | awk '$4 ~ /\.rs$/ {print}' | stream_sha256)
runner_commit=$(git log -1 --format=%H "$source_commit" -- "$runner_path")
contract_sha256=$(file_sha256 "$contract_copy")
inventory_sha256=$(file_sha256 "$inventory_copy")

jq -n \
  --arg schema norn.response_optional_lifecycle_gate.v1 \
  --arg generated_at "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" \
  --arg base_commit "$base_commit" --arg source_commit "$source_commit" \
  --arg source_tree "$source_tree" --arg rust_manifest_sha256 "$rust_manifest_sha256" \
  --arg runner_path "$runner_path" --arg runner_commit "$runner_commit" \
  --arg runner_sha256 "$runner_source_sha256" \
  --arg toolchain "$(rustc +1.94.0 --version)" --arg cargo_target_dir "$CARGO_TARGET_DIR" \
  --argjson gate_failures "$gate_failures" --argjson checks "$checks" \
  --arg contract_path "$contract_path" --arg contract_sha256 "$contract_sha256" \
  --arg inventory_path "$inventory_path" --arg inventory_sha256 "$inventory_sha256" \
  --argjson surface_count "$surface_count" --argjson anchor_count "$anchor_count" \
  --argjson unique_anchor_count "$unique_anchor_count" \
  --argjson resolved_anchor_count "$resolved_anchor_count" \
  --argjson policy_matches "$policy_matches_json" \
  --argjson loc_inventory "$loc_inventory" --argjson loc_violations "$loc_violations" \
  --argjson focused_passed "$focused_passed" --argjson focused_failed "$focused_failed" \
  --argjson focused_results "$focused_results" \
  '{schema: $schema, generated_at: $generated_at,
    result: (if $gate_failures == 0 then "pass" else "fail" end),
    gate_failures: $gate_failures,
    source: {base_commit: $base_commit, commit: $source_commit, tree: $source_tree,
      rust_manifest_sha256: $rust_manifest_sha256, frozen_committed_rust: true},
    runner: {path: $runner_path, commit: $runner_commit, sha256: $runner_sha256},
    environment: {toolchain: $toolchain, cargo_target_dir: $cargo_target_dir,
      os_temp_used: false},
    checks: $checks,
    official_contract: {path: $contract_path, sha256: $contract_sha256,
      output_items: 28, contextual_properties: 274, state_assertions: 659},
    lifecycle_inventory: {path: $inventory_path, sha256: $inventory_sha256,
      surfaces: $surface_count, anchors: $anchor_count,
      unique_tests: $unique_anchor_count, resolved_anchors: $resolved_anchor_count},
    diff_policy: {prohibited_added_lines: $policy_matches,
      production_loc_method: "prefix before first exact #[cfg(test)]",
      over_limit_policy: "an existing >=500-line production prefix is accepted only when its exact prefix hash is unchanged from the phase base",
      touched_rust: $loc_inventory,
      changed_production_prefix_violations_at_or_above_500: $loc_violations},
    focused_lifecycle: {passed: $focused_passed, failed: $focused_failed,
      sensitive_iterations: 20, tests: $focused_results}}' >"$partial"
mv "$partial" "$output"
printf '%s\n' "$output"

if [[ $gate_failures -ne 0 ]]; then
  exit 1
fi
