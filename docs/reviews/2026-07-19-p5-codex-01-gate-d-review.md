# P5 `CODEX-01` Gate D review

**Date:** 2026-07-19

**Reviewer:** Sable Nightwick (coordinator, Fable) + two Opus area seats
(provider/wire; loop/session/CLI) + norn cross-model pass (GPT-5.6 Sol,
read-only, session `3b5ad5b2-3453-4d19-aa9e-56b9cae90196`)

**Handoff:**
[`2026-07-19-p5-codex-01-gate-d-handoff.md`](2026-07-19-p5-codex-01-gate-d-handoff.md)

**Reviewed head:** `a1afcd6` (product `98b0266` tree `7d14354` over base
`62a852e`; test-only correction `fcaf0e4`, combined tree `b8835d0`)

## Verdict

**CODEX-01: READY as an implementation candidate, contingent on one MINOR
test-binding correction (M-1 below) landing before merge.** No product defect
was found by any seat, by the cross-model pass, or by my own adversarial
checks — every reviewer question closes in the candidate's favor. The one
genuine finding is a test whose assertions do not bind its named invariant.
Whole-P5 acceptance remains out of scope, as the handoff states.

TRANS-01 (`e448133`, already merged to main): I additionally read the product
diff and exercised its suites. The `TaskOwnedProviderStream` drop-abort wrapper
is sound (consumer drop aborts the producer task and thereby the in-flight
HTTP request; abort of a finished task is harmless), wired into both providers,
with real stall-server integration tests that pass in my battery. This is a
bounded coordinator verification, not a full panel; no defect found. Note the
`5959dfa` merge message's "reviewed" — no external review verdict document
exists for TRANS-01; this section is the first recorded external check.

## Findings

**M-1 (MINOR, CONFIRMED empirically — fix before merge):**
`custom_tool_call_precedes_an_explicit_continue_directive`
(`crates/norn/src/loop/runner/tests.rs`) does not bind the invariant it names.
Its assertions (final text, two provider calls, persisted assistant custom
call) all hold even when the actionable call is skipped in favor of
continuation, because `persist_assistant_turn` records the call regardless of
classification and the scripted second response completes either way. I proved
this by mutation: removing the `tool_calls.is_empty()` guard from
`classify.rs:119` — the exact regression the test exists to catch — leaves this
test green while `schema_call_precedes_an_explicit_continue_directive`
correctly fails (that sibling genuinely binds, since its single scripted
response can only complete by consuming the schema call). Credit: found by the
norn cross-model pass; confirmed by me end-to-end; worktree restored
byte-identical afterwards. Fix is small: assert handler invocation count
(exactly one) and/or a persisted `ToolResult` / custom-tool output in the
second request. The production guard itself is correct — this is evidence
debt, not a code defect.

No BLOCKER or MAJOR. No other MINOR.

## What was verified (all seats' load-bearing claims re-verified by me)

- **Trust chain (SOUND):** Codex dialect ⟺ `OpenAiBackend::CodexSubscription`
  ⟺ `AuthSource::OAuth`, and the endpoint is the compiled `CHATGPT_BASE_URL`
  constant regardless of configured overrides (`backend.rs:35-57`; OAuth
  base-url overrides are validated against the canonical ChatGPT shape or
  error; API-key auth cannot select the Codex endpoint). Untrusted config
  cannot reach Codex semantics, and Codex semantics cannot pair with a hostile
  base_url. This fully covers the SEC-01 class for the new dialect path.
- **Terminal matrix (SOUND):** all seven handoff rows verified row-by-row in
  `completed_stop_reason` (`sse_types.rs:80-103`), including public backend
  with `end_turn: null` present → typed parse failure with no `Done`
  (`Value::Null` is `is_some()` on presence check), Codex `null`/absent →
  `EndTurn`, and malformed values (`0`, `{}`, `"yes"`) failing closed *before*
  the actionable-call branch.
- **Actionable set (SOUND):** `FunctionCall | CustomToolCall` is exactly the
  set assembly records for local dispatch (`assembly.rs:302-331`, exhaustive
  match, schema call is an ordinary function call; `WebSearchCall` is hosted).
  The old deleted last-item-only inference missed `CustomToolCall` and
  non-last calls — the new any-match closes a pre-existing gap. Defense in
  depth: classify honors `ContinueTurn` only when `tool_calls.is_empty()`.
- **No bypass (SOUND):** the only production `map_sse_event` caller routes
  `response.completed`/`response.incomplete` to the dialect-aware
  `decode_terminal` first; a stream ending without a terminal surfaces as
  retryable `StreamInterrupted`, never silent success; `openai_compatible`
  uses its own decoder.
- **Continuation flow (SOUND):** `persist_assistant_turn` (write-through to
  the session sink — disk write before in-memory visibility,
  `store.rs:194-208` — fsync cadence per owner-configured `DurabilityPolicy`)
  completes before dispatch can reach the next provider call. `gate()` checks
  cooperative cancellation and `max_iterations` per continuation and
  increments the iteration counter; step-timeout wraps all iterations. The
  disclosed indefinite-continuation boundary is accurate and correctly
  un-defaulted per NO-ARBITRARY-LIMITS.
- **Refusal semantics (SOUND):** refusal-only + `ContinueTurn` continues with
  the canonical refusal item persisted and replayed into the next request
  (bound by test asserting `msg_continue_refusal` in `requests[1]`); refusal +
  any actionable call remains refusal-authoritative and executes nothing.
- **Schema budget (SOUND):** the `ContinueTurn` arm returns before all
  budget/nudge code; bound by the budget-1 test completing on the second call
  with no nudge message.
- **Persistence/replay (SOUND):** `stop_reason` is audit metadata only — no
  resume/replay code branches on it; pre-field events replay as `""`. Empty
  continuation persists an empty natural assistant event that serializes to
  zero wire items.
- **CLI projection (SOUND):** `StopReason` has no `Serialize`/`Display`, so
  every projection is an explicit exhaustive match; all three production
  matches handle `ContinueTurn`; no wildcard arm can mislabel it.
- **Policy (SOUND):** zero production `unwrap`/`expect`/`panic!`/bypass
  attributes across the diff; no invented limits; all production prefixes
  under 500 LOC; `fcaf0e4` genuinely removes the only added test `expect`.
- **My battery (this host, primary-repo target, network-capable):**
  `cargo fmt --check` clean; `cargo clippy --workspace --all-targets
  --all-features -- -D warnings` clean; full workspace test run **all green,
  zero failures** including the norn library's 4,041 tests the implementer's
  sandbox could not run (loopback bind restriction) — that gap in the
  implementer's evidence is now closed — plus norn-cli 501/501 and norn-tui
  683/683.

## Observations (non-blocking, carried)

1. **Duplicate JSON members collapse last-wins protocol-wide** (found by the
   norn pass as a major; adjudicated down): a raw Codex terminal carrying
   `"end_turn":false,"end_turn":true` reaches `completed_stop_reason` as a
   single surviving value because the SSE layer materializes
   `serde_json::Value` — the strict check cannot see duplicates. This is not a
   CODEX-01 defect: identical last-wins semantics apply to every field in the
   entire Responses pipeline (the Value-based architecture accepted through
   P3/P4), the pinned Codex reference client has the same behavior, and the
   arm is reachable only from the pinned chatgpt.com endpoint over OAuth.
   Rejecting duplicate members would be a whole-protocol decision at the SSE
   parser layer — an owner call, recorded here for the ledger.
2. **Incomplete-terminal asymmetry (disclosed):** `response.incomplete`
   ignores `end_turn` entirely — a public backend sending it there is not
   rejected (unlike completed), and Codex `incomplete` + `false` yields
   truncation, never continuation. Safe outcome, disclosed in the handoff;
   a one-line rejection would restore symmetry in a later slice.
3. **Layer seam in tests:** the runner suite hand-constructs
   `Done{ContinueTurn}` and the terminal unit tests supply
   `ReconcileUpdate::Terminal` directly; wire→`ContinueTurn` is bound at the
   mapper level (`mapper_honors_end_turn_only_for_the_codex_dialect`), but no
   single test drives raw `end_turn:false` bytes end-to-end to CLI output.
   Acceptable unit/integration seam; an end-to-end wiremock case would harden
   it.
4. **Durability-ordering evidence:** the continuation test binds
   append-before-next-request via request content, but does not observe the
   store at the moment the second `stream` call begins; the implementation
   ordering is verified at source (persist completes before dispatch). A
   store-observing provider fixture would bind the crash-ordering claim.
5. **Dead `response.incomplete → Done` arm** remains in `map_sse_event`
   (unreachable on the Responses path; pre-existing, test-exercised only).
6. **Empty-continuation wire payload** is proven empty by code trace
   (`serialize_assistant_into` emits nothing for an all-empty message), but no
   test inspects the second request's payload for it.

## Boundaries

- This verdict covers the CODEX-01 candidate (plus a bounded TRANS-01
  coordinator verification). It is not P5 acceptance. D3, D8, CODEX-02 turn
  state, client metadata, account-bound anchors, and whole-P5 evidence remain
  open.
- Merge is appropriate once M-1 lands; I will confirm the test correction
  narrowly on request (same-reviewer precedent: D2 F1, audio M-1).
