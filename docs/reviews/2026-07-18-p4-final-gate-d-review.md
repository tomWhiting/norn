# P4 whole-phase Gate D review — streaming and replay conformance

**Date:** 2026-07-18
**Reviewer:** Sable Nightwick (standing P3/P4 review coordinator)
**Handoff:** [`2026-07-18-p4-final-gate-d-handoff.md`](2026-07-18-p4-final-gate-d-handoff.md)
**Phase base:** `a90b730` · **Frozen source:** `7f47218` (tree `b8b042f6`)
**Exact range:** `a90b730..7f47218` · **P3 prerequisite:** READY at `06be7c7`

## Verdict

**P4 — NOT READY.** One confirmed, undisclosed, P4-owned implementation defect
(**MAJOR-1**) lets preview-only text that the provider never authoritatively
completed be promoted into the canonical transcript, the returned answer, strict
persistence, and `store:false` replay — a direct contradiction of `STATE-01`
("persists and replays as exact provider items") and of the reconciler's own
module contract ("canonical transcript items come exclusively from authoritative
frames"). It is reachable by a non-conforming or malicious provider endpoint,
which norn's threat model (SEC-01: repo-settable `base_url`) treats as in scope,
and which P4's `EVT-05` closure is specifically meant to fail closed against.

The defect is small and localized; a narrow correction plus regenerated final
evidence and a same-reviewer confirmation is the expected path (the D2 F1 / audio
M-1 precedent). This is a `P4-only` verdict; it does not disturb the accepted P3.

## MAJOR-1 (CONFIRMED end-to-end by the coordinator) — orphan preview text promoted into canonical truth

**Trigger (all frames schema-valid, accepted with zero errors):**

1. `response.output_item.added` — `output_index:0`, `item:{type:"message", id:"msg_1", role:"assistant", status:"in_progress", content:[]}`
2. `response.output_text.delta` — `item_id:"msg_1"`, `output_index:0`, `content_index:0`, `delta:"<preview text>"`, `logprobs:[]`
3. `response.completed` — `response:{status:"completed", output:[], usage:{…}}`

**Mechanism, each step verified by the coordinator's own read at `7f47218`:**

- `message` is `Inert` (`response_contract.rs:272`).
- The bare `output_text.delta` is accepted after only the announcement
  (`accept_delta`, `response_reconciler.rs:218-245`); it stores an accumulated
  delta in `self.deltas[(msg_1, OutputText)]` and does **not** mark
  `item_channels.touched` (nothing in `accept_delta` touches it).
- `finish()` (`response_reconciler.rs:355-391`) returns `Terminal { items:[] }`
  with **no error**. None of its three orphan validators covers this cell:
  `validate_terminal_channels` inspects only `completed_channels` (populated by a
  `.done`, which never arrived — `channels.rs:104-114`);
  `validate_terminal_item_channels` inspects only `item_channels.touched`
  (unmarked — `item_channels/authority.rs:54-66`); `validate_actionable_resolution`
  inspects `self.deltas` for **only** `FunctionCallArguments|CustomToolCallInput`
  channels and treats an orphaned `Inert` announcement as `{}`
  (`response_reconciler.rs:457-489`).
- The mapper emits no `ResponseItemDone` for the empty terminal list but does
  emit `Done` (`execute.rs:168-211`); the earlier delta already emitted a
  `TextDelta` preview (`execute.rs`, `map_sse_event`).
- Assembly keeps the delta-accumulated `text` because
  `completed_message_projection(&[])` returns `None`, so the overwrite at
  `assembly.rs:257-260` never fires; `AssembledResponse{ text:"<preview>",
  response_items:[] }`.
- Classification returns success: no refusal, no tool calls, `EndTurn` →
  `TextStopNoSchema` (`classify.rs:110-141`); `on_text_stop` returns the phantom
  as the agent's text output (`runner/dispatch.rs`, no-schema branch).
- Replay fabricates provider input: with `response_items` empty,
  `serialize_assistant_into` takes the legacy path and echoes an `output_text`
  message the authoritative terminal `output:[]` never contained
  (`request.rs:370-401`).

**Why it is a defect, not a benign shape.** Every neighboring cell of this exact
class fails closed and is tested — the `.done` channel orphan yields
`ChannelCompletionAbsentFromTerminal` (`channels.rs:320-336`, terminal `output:[]`),
the completed-item orphan yields `CompletionAbsentFromTerminal`
(`terminal.rs:143-155`), the item-scoped `touched` orphan yields
`ItemScopedStateAbsentFromTerminal`, and a delta-only actionable call yields
`DeltaOnlyActionableCall` (`terminal.rs:176-193`). The one uncovered cell is a
bare `output_text.delta` (never a `.done`, never a `content_part.added`) on an
`Inert` item absent from terminal — and it fails **open**. No test exercises it;
no handoff line or observation-ledger entry discloses it. On a conforming wire
`content_part.added` precedes text deltas and marks item-scoped state, so a
real dropped item already fails via the touched check — only the bare-delta
shape (exactly the hostile shape a malicious `base_url` can send) slips through.
This contradicts `STATE-01`, the `EVT-04` atomicity claim ("preview divergence
beyond a missing suffix fails atomically" — here the divergence is total and is
accepted), and the spirit of `EVT-05` ("unknown/malformed wire produces a typed
failure before an ordinary assistant turn is published").

**Fix.** In `finish()`, require every identity that holds accumulated
core-channel deltas (`OutputText`/`Refusal`/`ReasoningText`/`ReasoningSummaryText`)
to appear in the terminal identity set — symmetric with the existing
`completed_channels`, `touched`, and actionable-delta guards — yielding a typed
error. Alternatively, the owner may explicitly rule the malformed-provider shape
out of scope and disclose it as an accepted residual; given the SEC-01 threat
model I recommend the fix.

## MINOR-2 (CONFIRMED) — post-terminal duplicate is turn-fatal, contradicting the EVT-06 idempotence wording

`map_event` returns `PostTerminalFrame` for **any** frame once `self.terminal`
is set (`execute.rs:83-87`), before the reconciler's duplicate detection runs.
The reconciler alone treats an exact duplicate of the terminal frame as
idempotent (`tests/sequence.rs:148`), and the handoff's `EVT-06` states "exact
duplicates are idempotent"; but `call_provider` propagates the mapper `Err`
(`classify.rs:243-244`), so an app-layer retransmit of the terminal frame turns
a completed response into a terminal `ResponseProtocolViolation`. It fails
**closed** (integrity preserved), so this is a claim-accuracy/robustness nit, not
an integrity hole — carry it into the correction: either guard the exact
terminal-frame duplicate at the mapper or align the `EVT-06` wording.

## What held (the rest of the panel)

Four seats plus a cross-model pass; the three Opus area seats and my own checks
found the closure otherwise systematic and honest, and the adversarial seat's own
strongest attacks (refusal→success, double-repair, partial publication,
duplicate/interleave/rebind) were all defeated.

- **Streaming/item (Opus): SOUND ×5; STATE-01, EVT-02, EVT-05, EVT-06, EVT-07
  CLOSED** — except for the MAJOR-1 cell within STATE-01/EVT-02. Manifest pinned
  by type (53/28/18, compile-fail on wrong length) plus assertions; Codex overlay
  provably disjoint; identity never fabricated and rebinding fails closed both
  directions; double executable gate (reconciler + assembly).
- **UI/session (Opus): SOUND ×5; EVT-01, EVT-04 CLOSED** — suffix-only preview
  repair rejects any non-prefix divergence with atomic pre-compute; refusal is
  classified first and terminates non-retryably, surviving persist/reload/resume
  without flat substitution; TUI makes all completion events no-ops (no
  duplication); CLI/TUI files clean on CLAUDE.md. (MAJOR-1 is the reconciler-level
  hole beneath the UI layer this seat correctly found sound at its own layer.)
- **Capability-survival (Opus): SOUND ×5; EVT-03 CLOSED** — hosted-search →
  local-tool continuation → `store:false` resume proven in exact order by two
  independent tests; per-capability × per-seam table shows no drops; streamed vs
  terminal-only reconciliation produces the same vector under out-of-order and
  duplicated frames; request-side replay confirmed backend-independent.
- **Cross-model pass (norn CLI, gpt/Codex model, read-only tools):**
  independently corroborated that optional-ID fabrication and identity rebinding
  are prevented; did not find MAJOR-1 (it attacked `function_call` shapes); its
  headline "sequence-gap" concern is the already-known by-design residual (text
  fails closed via the prefix rule; audio is a best-effort receive-only sidecar).

## Coordinator machine-evidence reproduction

The frozen source, tree, and all five evidence hashes are byte-identical to the
P3 review of the same commit; the full source-bound battery (workspace 5,364,
doctests 8, distributions 60/60, redaction sentinels 23/23, clippy/fmt, policy
zero-violations, 350-path inventory) was reproduced byte-exact at `7f47218`
during the P3 whole-phase review and is unchanged here — a re-run of the
identical tree is unnecessary. D15 (§21) is owner-approved and honestly scopes
the deterministic fixtures as sufficient for the P4 gate while keeping the
authenticated real-wire test mandatory at D7/P9 ("a skipped test is never a
pass").

## Standing / required for P4 to close

Per the handoff's own rule, MAJOR-1 is a source correction that invalidates the
freeze: it requires a new frozen source and regenerated final evidence, then a
narrow same-reviewer confirmation of MAJOR-1 (and MINOR-2). `EVT-01`, `EVT-03`,
and `EVT-07` are closed; `STATE-01`, `EVT-02`, `EVT-04`, and `EVT-05` are closed
except for the MAJOR-1 cell and close fully once the orphan-delta guard lands.
Independently of this review, the mandatory D7/P9 authenticated real-wire gate
remains open before overall integrated Responses acceptance.

## Observations carried (non-blocking)

OBS — `response.failed` skips actionability enforcement but is verified safe (the
mapper returns `Err`, the loop aborts, assembly never dispatches); independently
noted by the P3 protocol seat. OBS — the "16 tool schemas" figure is not
assertion-pinned (only the 18-literal count is). OBS — plain-text/TUI refusal is
not visually styled distinctly from normal text (by design; refusal is a
legitimate model outcome). OBS — hosted-search citations reach the CLI as raw
`response_item` passthrough, not a dedicated citation renderer (survival, not
rendering, is the claim). OBS — forward `sequence_number` gaps are tolerated
(text fails closed at completion via the prefix rule; audio is a best-effort
sidecar). Plus the carried D2/audio/D11 ledger, unchanged.
