# P5 `CODEX-02` BLOCKER-1 correction confirmation

**Date:** 2026-07-19

**Reviewer:** Sable Nightwick (same reviewer as the CODEX-02 Gate D verdict)

**Controlling review:**
[`2026-07-19-p5-codex-02-gate-d-review.md`](2026-07-19-p5-codex-02-gate-d-review.md)
(`b86924d`, NOT READY on BLOCKER-1)

**Correction commit:** `de92211` (two source paths over the review commit)

**Confirmed head:** `343ad42` on `codex/p5-codex-turn-state`

**Correction tree:** `43374607900ba95f8c64b9b6507d102d356e11a8`

## Verdict

**BLOCKER-1 CLOSED. CODEX-02 is now READY as an implementation candidate.**
Whole-P5 acceptance remains out of scope; the four non-blocking observations
from `b86924d` carry unchanged.

## Confirmation checklist

1. **Recursive, structure-independent redaction — CONFIRMED.**
   `redact_codex_turn_state` (`crates/norn/src/provider/turn.rs`) now delegates
   to a recursive in-place walker: every object key is compared
   case-insensitively to `x-codex-turn-state` and its complete value subtree is
   replaced with `[REDACTED]`; non-matching object values and every array
   element are traversed; scalars are inert. Redaction is key-based, not
   path-based, and no longer depends on a top-level object `headers`. The unit
   test `metadata_accepts_case_and_first_array_string_then_redacts` now covers
   array-shaped headers, a nested-object/array occurrence, mixed-case keys, and
   a sensitive key under a field *not* named `headers` (`outside_headers`).

2. **Neither sink retains the sentinel — CONFIRMED by end-to-end reproduction
   and mutation.** The new integrated regression
   `noncanonical_metadata_redacts_turn_state_from_every_output_sink`
   (`turn_state_tests.rs`) is exactly my reproduced exploit: a 2xx response whose
   `x-codex-turn-state` header captures the authoritative live secret, plus a
   `response.metadata` body carrying the same sentinel under the sensitive key
   in an array and in a nested-object/array with mixed-case spelling. It drives
   the real `SenderProvider`, `StreamExecutor`, SSE parser, `ResponsesMapper`,
   provider-event channel, and an enabled on-disk `DebugDumper`, then asserts the
   captured context still holds the header secret while the emitted raw envelope,
   the full `ProviderEvent` debug output, and the debug JSONL contain no
   sentinel (and carry `[REDACTED]`). It passes. **Mutation kill:** reverting the
   redactor to the reviewed shallow object-only lookup fails both this regression
   (its first raw-envelope secrecy assertion observes the sentinel) and the unit
   test; restoring the recursive walker restores both. The tests genuinely bind
   the fix. Worktree restored byte-identical after each mutation.

3. **Real transport coverage — CONFIRMED.** The regression exercises the actual
   HTTP/SSE path (wiremock 200 → real parser → mapper → channel) and a real
   temp-file `DebugDumper`, not a hand-built envelope.

4. **No behavior changed outside BLOCKER-1 — CONFIRMED.** The correction is two
   files (`turn.rs`, `turn_state_tests.rs`). The recursive walker replaces only
   the redaction helper, which is called solely from the two disclosure
   boundaries (`write_sse_event` and the emitted-envelope construction). Metadata
   capture (`codex_turn_state_from_metadata`), first-wins authority
   (`observe_codex_turn_state`), replay (`codex_turn_state_header`), envelope
   validation, trust-gating, and request construction are untouched. Capture
   remains narrow (top-level object only) while redaction is now broad, so
   redaction strictly covers capture — the safe direction.

5. **Claims do not exceed the diff — CONFIRMED.** The NUL-delimited two-path
   inventory for `b86924d..de92211` reproduces to
   `90770101985689ac6ff8c446cde7adfd2a31ae0ed91daf61e84114c3f0412edf`, and
   `de92211^{tree}` is `43374607900ba95f8c64b9b6507d102d356e11a8`, both matching
   the handoff.

## My battery (network-capable, primary-repo target, at `343ad42`)

`cargo fmt --all -- --check` clean; `cargo clippy --workspace --all-targets
--all-features -- -D warnings` clean; full workspace green — norn 4,052 / cli
501 / tui 683, matching the implementer's reported 5,406. The seven turn-state
tests pass, including the new regression.

## Boundaries

- BLOCKER-1 is the only item this confirmation covers. The four non-blocking
  observations from `b86924d` (case-sensitive protected-key rejection; public
  `Default` constructor; `from_sse` fatal on a metadata frame missing its
  `event:` line; ASCII-formatter parity not blob-diffed) remain carried.
- D3, D8, account affinity, resume/concurrency isolation, WebSocket state
  transport, and the mandatory D7/P9 authenticated real-wire gate remain open.
- This is a CODEX-02 candidate confirmation, not P5 acceptance. Merge is
  appropriate; acceptance remains the owner's action.
