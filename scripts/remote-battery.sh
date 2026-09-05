#!/usr/bin/env bash
# Diagnostic checks only. Landing requires the terminal receipt from the
# exact-commit repo_battery_205 workflow, including its measured leg verdicts.
# Workflow success alone is not a passing receipt. These logs are not one.
#
# Every leg runs after earlier failures, with raw output and exit status kept
# in gate-logs/. Formatting only checks; this script never restores source.
# Refuse an initially dirty tracked tree before starting any diagnostic leg.
# Usage from a clean candidate checkout: bash scripts/remote-battery.sh

set -euo pipefail
printf '%s\n' 'DIAGNOSTICS ONLY — not a landing receipt; use repo_battery_205 on the exact commit.'
for required in dirname git cargo rustc sed mkdir hostname uname date cat; do
  if ! command -v "$required" > /dev/null 2>&1; then
    printf 'REFUSAL: required command unavailable: %s\n' "$required" >&2
    exit 3
  fi
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! TRACKED_STATUS="$(git status --porcelain --untracked-files=no)"; then
  printf '%s\n' 'REFUSAL: cannot establish tracked-tree cleanliness.' >&2
  exit 3
fi
if [[ -n "$TRACKED_STATUS" ]]; then
  printf 'REFUSAL: tracked tree is dirty; source was not changed:\n%s\n' "$TRACKED_STATUS" >&2
  exit 3
fi

LOGDIR="gate-logs"
mkdir -p "$LOGDIR"
if ! PINNED="$(sed -n 's/^channel *= *"\([^"]*\)" *$/\1/p' rust-toolchain.toml)" \
    || [[ -z "$PINNED" ]]; then
  printf '%s\n' 'REFUSAL: cannot read the pinned channel from rust-toolchain.toml.' >&2
  exit 3
fi
if ! RUSTC_VERSION="$(rustc --version 2>&1)"; then
  printf 'REFUSAL: rustc --version failed: %s\n' "$RUSTC_VERSION" >&2
  exit 3
fi
read -r COMPILER ACTIVE_VERSION VERSION_TAIL <<< "$RUSTC_VERSION"
if [[ "$COMPILER" != rustc || "$ACTIVE_VERSION" != "$PINNED" ]]; then
  printf 'REFUSAL: running compiler (%s) does not match pinned channel %s\n' \
    "$RUSTC_VERSION" "$PINNED" > "$LOGDIR/toolchain-mismatch.refusal"
  cat "$LOGDIR/toolchain-mismatch.refusal" >&2
  exit 3
fi

# Capture commands separately: a successful printf must not hide a failed
# toolchain or Git probe inside command substitution.
HOSTNAME_VALUE="$(hostname)"
UNAME_VALUE="$(uname -a)"
STARTED_AT="$(TZ=Australia/Melbourne date '+%Y-%m-%dT%H:%M:%S%z')"
CARGO_VERSION="$(cargo --version 2>&1)"
HEAD_COMMIT="$(git rev-parse HEAD)"
{
  printf 'authority:  diagnostics only; not a landing receipt\n'
  printf 'hostname:   %s\n' "$HOSTNAME_VALUE"
  printf 'uname:      %s\n' "$UNAME_VALUE"
  printf 'Melbourne:  %s\n' "$STARTED_AT"
  printf 'rustc:      %s\n' "$RUSTC_VERSION"
  printf 'cargo:      %s\n' "$CARGO_VERSION"
  printf 'toolchain:  %s\n' "$PINNED"
  printf 'git-head:   %s\n' "$HEAD_COMMIT"
  printf 'git-dirty:  no tracked changes at admission\n'
} > "$LOGDIR/environment.txt"
cat "$LOGDIR/environment.txt"

# Tests must not touch the operator's real ~/.norn. This explicit diagnostic
# path is local to the candidate, and is not a venue concurrency setting.
export NORN_HOME="$ROOT/target/remote-battery-norn-home"
mkdir -p "$NORN_HOME"

declare -a LEGS=()
DIAGNOSTIC_RED=0
run_leg() {
  local name="$1"
  local rc
  shift
  LEGS+=("$name")
  printf '== leg: %s\n' "$name"
  if "$@" > "$LOGDIR/$name.log" 2>&1; then
    rc=0
  else
    rc=$?
  fi
  printf '%s\n' "$rc" > "$LOGDIR/$name.exit"
  printf '== leg %s exit=%s\n' "$name" "$rc"
  if [[ "$rc" != 0 ]]; then
    DIAGNOSTIC_RED=1
  fi
}

run_leg fmt cargo fmt --all -- --check
run_leg clippy cargo clippy --locked --workspace --all-targets -- -D warnings
run_leg tests cargo test --locked --workspace --all-targets --no-fail-fast
run_leg doctests cargo test --locked --workspace --doc --no-fail-fast

# Keep aggregation in this shell. A pipeline into tee would put the loop in
# a subshell, losing a failure flag set inside it.
{
  printf 'diagnostic summary — %s Melbourne\n' "$STARTED_AT"
  for leg in "${LEGS[@]}"; do
    rc="$(cat "$LOGDIR/$leg.exit")"
    printf '  %s: exit %s\n' "$leg" "$rc"
  done
  printf 'Landing authority: exact-commit repo_battery_205 terminal receipt, not these diagnostics.\n'
} > "$LOGDIR/summary.txt"
cat "$LOGDIR/summary.txt"

if [[ "$DIAGNOSTIC_RED" != 0 ]]; then
  printf '%s\n' 'DIAGNOSTICS RED — inspect gate-logs/; no landing claim.'
  exit 1
fi
printf '%s\n' 'DIAGNOSTICS GREEN — not a landing receipt; venue measurement is still required.'
