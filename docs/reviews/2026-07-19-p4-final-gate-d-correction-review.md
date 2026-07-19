# P4 final Gate D correction confirmation

**Date:** 2026-07-19

**Reviewer:** Sable Nightwick (same reviewer as the historical P4 verdict)

**Handoff:**
[`2026-07-19-p4-final-gate-d-correction-handoff.md`](2026-07-19-p4-final-gate-d-correction-handoff.md)

**Historical verdict:** `NOT READY` at
[`80f0e36`](2026-07-18-p4-final-gate-d-review.md)

**Confirmed head:** `84949b2c9244b0be87df39f1b7c8efef92213c1c`

**Evidence source:** `180759f506d9ca365999dab645794a8fc833a44f`, tree
`c588d96639cd6e7725255779afd445d479cbcd69`, base
`a90b730091bccaeaa03ba98c3b31425e40e32dac`

## Verdict

**MAJOR-1 CLOSED; MINOR-2 CLOSED; corrected P4 Gate D READY.**

This is the narrow same-reviewer confirmation prescribed by the historical P4
review. It is a P4-only verdict. It does not itself record P4 acceptance; that
remains the owner's checklist action. No new production seam was found, so no
broader re-review was convened.

## MAJOR-1 confirmation

The historical defect: a bare core preview delta on an announced item absent
from a successful terminal `output` array silently promoted phantom preview
text into the canonical transcript, persistence, and replay.

Confirmed closed at source and by experiment:

- `validate_terminal_core_deltas` (`response_reconciler/channels.rs`) rejects
  any accumulated delta identity on the `OutputText`, `Refusal`,
  `ReasoningSummaryText`, or `ReasoningText` channels that is absent from the
  authoritative terminal identity set, returning the typed, payload-free
  `CoreDeltaAbsentFromTerminal`. These four channels plus
  `FunctionCallArguments` are the complete `ResponseDeltaChannel` space;
  function-call arguments were already failed closed by
  `validate_actionable_resolution`, so no delta channel can orphan.
- The guard is called in the single terminal ingest path under
  `enforce_actionable_resolution`, which is true for `response.completed` and
  `response.incomplete` and false only for `response.failed`. It inspects
  pre-reconciliation delta state (`self.deltas` before the drained clone is
  written back), so identities already reconciled cannot mask orphans.
- The mapper-level regression
  (`terminal_boundaries::bare_preview_absent_from_terminal_fails_before_done`)
  reproduces the exact historical exploit: announced in-progress message, bare
  `output_text.delta`, `response.completed` with empty `output` — the raw
  stream event is preserved, then the typed violation, with no
  `ResponseItemDone` and no `Done`.
- **Mutation kill:** I removed the single guard call, re-ran, and both the
  reconciler matrix test and the mapper exploit test failed; restoring the
  call restored 7/7 targeted passes. The tests bind the guard; they are not
  vacuous. The worktree was restored byte-identical afterwards.
- The four-channel × completed/incomplete matrix
  (`terminal_output_cannot_omit_any_core_preview_delta_identity`) passes, and
  the error message leaks no item identifier.
- Valid terminal authority without an intermediate channel-done event remains
  accepted for all four channels
  (`terminal_authority_may_supply_core_preview_without_intermediate_done`).
- `response.failed` authority is not masked: the empty-output fixture pins the
  typed 503-class provider failure with no canonical item and no `Done`
  (`failed_response_remains_authoritative_over_orphan_preview`,
  `failed_terminal_preserves_failure_authority_over_orphan_preview`), and
  authoritative failed-response output items remain preserved before the
  typed failure.

## MINOR-2 confirmation

The overbroad duplicate-idempotence claim is narrowed everywhere it appeared
(`RESPONSES-API-REMEDIATION-PLAN.md` status, invariants, work checklist, test
checklist; `DECISIONS-2026-07.md` D15 addendum) to exactly the behavior
provided: exact duplicate sequences before terminal delivery are
raw-observable canonical/actionable no-ops; the first terminal closes the
request; direct post-terminal mapper input — including an identical terminal
retransmit — returns `PostTerminalFrame`; conflicting duplicates and identity
rebinding fail closed. No claim of universal post-terminal idempotence
survives. Behavior evidence:
`mapper_rejects_even_an_exact_terminal_retransmit_after_delivery` (direct
mapper) and `duplicate_terminal_wire_frames_deliver_one_terminal_outcome`
(wiremock provider stream: duplicated terminal bytes yield exactly one raw
terminal event, one canonical item, one `Done`, one usage result, one
response ID, no error). No product behavior was changed to satisfy wording.

## Correction hygiene

- `80f0e36..ab26632` is product-only: the guard, the error variant, and tests
  (397 insertions, no deletions outside test module registration).
- `ab26632..180759f` is evidence-scanner-only: the `private_prompt_content`
  false-positive exemption applies only when an entire string value is exactly
  a redaction rule identifier; the new negative sentinel proves contextual
  private content beside a rule name still fails
  (`test_rule_identifier_does_not_exempt_private_content`).
- `180759f..8faf1f4` adds only the five artifacts; `8faf1f4..84949b2` is
  documentation only. No P3 product source changed; P3 acceptance at
  `06be7c7` is not reopened.

## Independent evidence reproduction

- All five artifact SHA-256 hashes recomputed by me and identical to the
  handoff table; the attestation binds the other four hashes, `passed: true`,
  zero errors, correct source/tree.
- The 360-path NUL-delimited inventory hash
  (`1a0eff5dfaa45e7da2676ce98461f1e65ad713629e585a328705cf578bb58187`)
  reproduced from `git diff --name-only -z a90b730..180759f` on my machine;
  path count 360; `180759f^{tree}` = `c588d96…` confirmed.
- Policy: 299 changed Rust files, 78 test-only, zero violations, zero module
  shape or over-500 findings. Distributions: the three concurrency cases at
  20/20 each, 60/60. Redaction: 219 records, zero findings, source-bound.
- Gate environment: pinned cargo 1.94.0, sanitized controls, evidence output
  under the primary repository `<repo>/target` — no OS temporary and no
  worktree-local build directory.
- My own gate run at `84949b2` (primary-repo target): `cargo fmt --check`
  clean; `cargo clippy --workspace --all-targets --all-features -- -D
  warnings` clean; workspace all-target all-feature tests **5,371/5,371**
  (per-binary sums match the bundle exactly); redaction sentinels pass; the
  seven targeted correction tests pass individually.

## Boundaries carried unchanged

- P3 remains accepted at `06be7c7` for source `7f47218`.
- The historical P4 review's non-blocking observation ledger is carried
  unchanged.
- D15 is unchanged: the authenticated real-wire conformance test was not run
  and remains a mandatory D7/P9 gate before overall integrated Responses
  acceptance; it is not approval to use credentials or incur spend, and a
  skipped live test is never a pass.
- P4 acceptance is the owner's action on this verdict; nothing here accepts
  P2, P9, or any other open phase.
