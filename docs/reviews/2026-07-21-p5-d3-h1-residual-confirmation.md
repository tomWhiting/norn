# P5 D3 H1 residual correction confirmation

**Date:** 2026-07-21

**Reviewer:** Sable Nightwick (same reviewer as the D3 verdict and the correction
confirmation)

**Controlling confirmation:**
[`2026-07-21-p5-d3-gate-d-correction-confirmation.md`](2026-07-21-p5-d3-gate-d-correction-confirmation.md)
(`0dc2035`, D3 READY; three nonblocking H1 residuals)

**Correction handoff:**
[`2026-07-21-p5-d3-h1-residual-correction-handoff.md`](2026-07-21-p5-d3-h1-residual-correction-handoff.md)

**Confirmed head:** `e03baa8` on `codex/p5-d3-correction` (product correction
`e96ee64`, source `c8619ae`, tree `e7f676a1…`). **`main` untouched (`35d0b3d`).**

## Verdict

**H1-a CLOSED; H1-b CLOSED; H1-c CLOSED.** All three residual gaps I documented in
the correction confirmation are closed and mutation-verified. D3's existing
**READY** implementation-candidate verdict at `0dc2035` stands unchanged. "H1
CLOSED" may now be stated in full, within the correction's honestly-bounded scope
(appends routed through `EventStore` and the concrete Norn JSONL sink paths;
direct embedder-owned sink calls and trusted preloads are explicitly and correctly
outside the claim — the public `PersistenceSink` trait cannot police an embedder
calling its own implementation directly).

## H1-a — CLOSED (signed zero distinguished; mutation-verified)

`hash_number` (`response_publication_commitment.rs`) no longer special-cases zero;
it hashes `value.to_string()` under the existing type-tagged, length-framed digest,
so `-0.0` → `"-0.0"` and `0.0` → `"0.0"` yield different commitments. This makes the
digest strictly stronger than the `RetryPlanner`'s serde-`Number` value-equality
(which still treats `-0.0 == 0.0`): a sign-flipped divergent retry now fails closed,
while an identical retry (same bytes) still verifies — no false reject.

**Mutation kill (mine):** restoring the old `-0.0/+0.0 → "0.0"` normalization fails
`canonical_commitment_sorts_nested_objects_and_distinguishes_signed_zero`; restored
byte-clean.

## H1-b — CLOSED (managed single and batch appends gated; mutation-verified)

`EventStore::append` and `EventStore::append_batch` both call
`validate_response_publication_transition` → `validate_response_publication_append`
**before** any sink call or in-memory push, on both the sink-backed and sinkless
routes. The validator validates the requested batch
(`validate_new_response_publication_batches`) and then examines only the trailing
part of the existing timeline that could still complete a group. The trailing scan
starts at `existing.len() - (RESPONSE_PUBLICATION_MAX_GROUP_LEN - 1)` (last three
rows) and clones at most that suffix — **bounded O(1)**, no history-length-dependent
regression (reviewer Q5 satisfied). Custom sinks reached through `EventStore` are
therefore covered.

The pre-existing malformed-history fixtures now build through
`append_unvalidated_for_test`, which is `#[cfg(test)]`-gated (`store.rs:4`),
crate-private, **refuses every sink-backed store**, retains duplicate-ID rejection,
and has no production caller — a permitted test-only invalid-fixture mechanism, not
a production escape from the gate.

**Mutation kill (mine):** neutering `validate_response_publication_transition` fails
`custom_store_single_append_rejects_legacy_orphan_completion`
("custom sink completed a legacy orphan"),
`custom_store_rejects_suffix_only_legacy_orphan_completion`
("sinkless store completed a legacy orphan"), and
`single_append_rejects_response_boundaries_before_sink_or_memory_mutation`, while the
JSONL test still passes (independent sink-level layer); restored byte-clean.

## H1-c — CLOSED (durable legacy orphan completion refused; mutation-verified)

`validate_no_incomplete_legacy_response_publications` rejects any incomplete legacy
`ResponseStatePublication` group in the durable timeline read by the concrete Norn
JSONL writers — enforced at the singleton `persist` path (`jsonl_sink.rs:350`), the
multi-row batch path after the strict read and before retry-prefix/counter/writes
(`jsonl_sink/batch.rs:46`), and on the registered-timeline route. So a durable
legacy orphan (a pre-correction crash artifact) cannot be completed by a suffix
append, even when the in-memory `EventStore` does not hold it. The check is limited
to *incomplete* legacy groups: a complete legacy group remains readable and may
precede a newly committed V1 group, and an interrupted V1 group still completes only
by an exact retry whose committed boundary verifies the candidate. The added scan
rides on the strict read the JSONL append already performs, so it introduces no new
asymptotic cost class.

**Mutation kill (mine):** neutering
`validate_no_incomplete_legacy_response_publications` fails
`jsonl_sink_rejects_suffix_only_legacy_orphan_completion_without_writes`
("JSONL sink completed a legacy orphan") while the `custom_store_*` tests still pass
(caught by the EventStore-level layer); restored byte-clean.

## Confirmation checklist

1. **H1-a distinguishes signed zero, mutation-bound — CONFIRMED.**
2. **H1-b rejects invalid single/batch transitions before sink or memory mutation
   for sinkless and custom `EventStore` routes — CONFIRMED** (mutation-killed; the
   validator precedes `persist`/push in every branch).
3. **H1-c rejects incomplete legacy prefixes through singleton, multi-row, and
   registered JSONL routes without writes — CONFIRMED** (mutation-killed).
4. **Complete legacy history remains compatible; exact V1 retry semantics unchanged
   — CONFIRMED** (rejection limited to incomplete legacy; complete groups pass
   `response_publication_group_len`; V1 retry path intact; battery green).
5. **Trailing scan bounded by the four-row invariant; no append regression —
   CONFIRMED** (`existing.len() - 3` start; O(1)).
6. **Retained gates reproduce at the exact source — CONFIRMED** (my battery below
   matches).
7. **Guarantee not extended to direct embedder-owned sink calls or unvalidated
   trusted preloads — CONFIRMED as honestly disclosed.**

## My battery (network-capable, repository target, at `e03baa8`)

`cargo fmt --all -- --check` clean; `cargo clippy --locked --workspace --all-targets
--all-features -- -D warnings` clean, no suppression; full workspace green — **5,591
passed, 0 failed** (norn all-feature lib 4,219, CLI 518, TUI 683, and every other
binary), doctests 8/8 — matching the handoff. `append_unvalidated_for_test` is
`#[cfg(test)]`-only with no production caller.

## Boundaries

- This confirmation closes only H1-a/b/c. D3's `READY` implementation-candidate
  verdict at `0dc2035` is unchanged; owner acceptance and merge remain the owner's
  action.
- Out of scope, unchanged: D8 role authority, whole-P5 and P2 acceptance, WebSocket
  transport, the broader conversation-state matrices, the pre-existing non-driven
  headless exit-class follow-up, and the mandatory D7/P9 authenticated real-wire
  gate.
