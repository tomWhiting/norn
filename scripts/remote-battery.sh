#!/usr/bin/env bash
# Remote gate battery — evidence-grade verification on a machine that is not
# the landing machine. Written for the land/triple-candidate pipeline
# (Waffles' evidence standard, Jul 26 2026); generic to any norn candidate.
#
# Contract:
#   - Runs EVERY leg regardless of earlier reds; a battery that stops at the
#     first failure destroys the evidence of the later legs.
#   - Writes gate-logs/: one .log (raw output) + one .exit (raw exit code)
#     per leg, plus environment.txt stamping hostname, toolchain, HEAD, and
#     tree cleanliness (the two-machines law).
#   - Never mutates the candidate: the fmt leg applies `cargo fmt --all`
#     (no --check theatre, per estate law) and then treats any resulting
#     diff as the leg's red, restoring the tree before continuing.
#   - Exits non-zero if any leg is red. The operator then commits gate-logs/
#     on the candidate branch and pushes; the landing decision stays at the
#     verifier's hands.
#
# Usage, from the repo root on the fast device:
#   git fetch origin && git checkout land/triple-candidate
#   bash scripts/remote-battery.sh
#   git add gate-logs && git commit -m "evidence: remote battery" && git push

set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

LOGDIR="gate-logs"
mkdir -p "$LOGDIR"

# --- Environment stamp: who ran this, on what, against which bytes. --------
{
  echo "hostname:   $(hostname)"
  echo "uname:      $(uname -a)"
  echo "date-utc:   $(date -u +%FT%TZ)"
  echo "rustc:      $(rustc --version 2>&1)"
  echo "cargo:      $(cargo --version 2>&1)"
  echo "clippy:     $(cargo clippy --version 2>&1)"
  echo "toolchain:  $(tr '\n' ' ' < rust-toolchain.toml 2>/dev/null)"
  echo "git-head:   $(git rev-parse HEAD)"
  echo "git-dirty:  $(git status --short | grep -cv '^?? gate-logs') tracked entries dirty (gate-logs excluded)"
} > "$LOGDIR/environment.txt"
cat "$LOGDIR/environment.txt"

# The pinned toolchain must be the one actually running: a distro cargo that
# ignores rust-toolchain.toml is silent drift. Refuse to produce evidence
# from the wrong compiler rather than produce misleading evidence.
PINNED="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)"
if [ -n "$PINNED" ] && ! rustc --version | grep -q "$PINNED"; then
  echo "REFUSAL: running rustc ($(rustc --version)) does not match pinned toolchain $PINNED" \
    | tee "$LOGDIR/toolchain-mismatch.refusal"
  exit 3
fi

# Hermetic norn home: nothing this battery does may touch the operator's real
# ~/.norn (the session-store corruption incident is the reason this line exists).
export NORN_HOME="$ROOT/target/remote-battery-norn-home"
mkdir -p "$NORN_HOME"

# --- Legs. Each records raw output + raw exit; none stops the battery. -----
declare -a LEGS=()
run_leg() {
  local name="$1"; shift
  LEGS+=("$name")
  echo "== leg: $name"
  ( "$@" ) > "$LOGDIR/$name.log" 2>&1
  echo "$?" > "$LOGDIR/$name.exit"
  echo "== leg $name exit=$(cat "$LOGDIR/$name.exit")"
}

# fmt: apply, capture any diff as the red, restore. No mutation escapes.
run_fmt_leg() {
  LEGS+=("fmt")
  echo "== leg: fmt (apply + diff-capture)"
  cargo fmt --all > "$LOGDIR/fmt.log" 2>&1
  if git diff --quiet; then
    echo "0" > "$LOGDIR/fmt.exit"
  else
    {
      echo "cargo fmt --all produced changes — formatting was not clean:"
      git diff --stat
    } >> "$LOGDIR/fmt.log"
    echo "1" > "$LOGDIR/fmt.exit"
    git checkout -- .
  fi
  echo "== leg fmt exit=$(cat "$LOGDIR/fmt.exit")"
}

run_fmt_leg
run_leg clippy cargo clippy --workspace --all-targets -- -D warnings
run_leg tests  cargo test --workspace

# --- Summary manifest + overall verdict. -----------------------------------
RED=0
{
  echo "battery summary — $(date -u +%FT%TZ)"
  for leg in "${LEGS[@]}"; do
    rc="$(cat "$LOGDIR/$leg.exit")"
    echo "  $leg: exit $rc"
    [ "$rc" = "0" ] || RED=1
  done
} | tee "$LOGDIR/summary.txt"

if [ "$RED" = "1" ]; then
  echo "BATTERY RED — commit gate-logs/ anyway; red evidence is still evidence."
  exit 1
fi
echo "BATTERY GREEN — commit gate-logs/ on the candidate branch and push."
