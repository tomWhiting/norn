# P5 `CODEX-01` M-1 correction confirmation

**Date:** 2026-07-19

**Reviewer:** Sable Nightwick (same reviewer as the CODEX-01 Gate D verdict)

**Verdict document:**
[`2026-07-19-p5-codex-01-gate-d-review.md`](2026-07-19-p5-codex-01-gate-d-review.md)

**Correction commit:** `c1efffa` (test-only, over `b87d410`)

**Confirmed head:** `c1efffa` on `codex/p5-codex-turn`

## Verdict

**M-1 CLOSED. CODEX-01 is now unconditionally READY as an implementation
candidate; merge is appropriate.** Whole-P5 acceptance remains out of scope.

## What was checked

The correction is a 13-line, test-only change to
`custom_tool_call_precedes_an_explicit_continue_directive`
(`crates/norn/src/loop/runner/tests.rs`): the `apply_patch` handler now
increments an `AtomicUsize`, and the test asserts exactly one execution before
continuation. No production source changed (`git diff b87d410..c1efffa`
touches only that one test file).

The invariant is now genuinely bound, proven by mutation:

- **Baseline (unmutated):** `custom_tool_call_precedes...` and its sibling
  `schema_call_precedes...` both pass.
- **Mutation** — remove the `tool_calls.is_empty()` guard from
  `classify.rs:119` (the exact regression M-1 exists to catch, which silently
  skips an actionable call in favor of continuation): the test now **FAILS**
  with `assertion left == right failed: the custom tool must execute exactly
  once before continuation` (0 executions observed instead of 1). Before this
  correction the same mutation left the test green. The mutant is killed;
  the worktree was restored byte-identical afterwards.

Gate on the change: `cargo fmt --all -- --check` clean; `cargo clippy -p norn
--all-targets --all-features -- -D warnings` clean.

## Boundaries

All CODEX-01 Gate D observations carry unchanged. This confirmation covers only
the M-1 test-binding correction. D3, D8, CODEX-02 turn state, client metadata,
account-bound anchors, and whole-P5 evidence remain open.
