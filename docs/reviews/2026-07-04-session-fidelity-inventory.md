# Session-Persistence Fidelity Gap Inventory

> **STATUS (2026-07-07): campaign landed.** Gaps 1, 2, 3, 5, 6, 7, 8, 9,
> 10, 11, 12, 14 are CLOSED on main across three reviewed merges —
> `dc484f5` (events: 6/7/8/9), `ce3e822` (store: 5/10/12), `17c0eae`
> (tree/child-persistence: 1/2/3/11/14). Gap 4 (action-log persistence)
> is DEFERRED to its own unit (depends on the tree layout, now landed);
> Gap 13 is live-only by design. Embedder-visible schema changes are
> ticketed: meridian/docs/reviews/2026-07-07-norn-event-schema-pin-bump.md.
> Open owner ruling: EventId stays UUIDv7 (nothing sorts it; it surfaces
> in spool filenames) or joins the v4 unification.

Recon date: 2026-07-04. Verified at HEAD, main @ 3cac008. All paths relative to repo root.
Purpose: complete gap list between what happens in a norn agent run and what reaches durable
session storage. This inventory drives the session-tree storage design campaign (see the
2026-07-04 session-vision notes: tree sessions, road-sign annotations, layered context views).

## Architecture baseline (verified)

The only durable sink is `JsonlSink` (JSONL file + index registration), implemented in
`crates/norn/src/session/store.rs:197-366`, installed exclusively by `SessionManager`
(`crates/norn/src/session/manager.rs:536-547, 553-582`). `EventStore::append` is
write-through-before-memory (`store.rs:441-470`). Anything that reaches a sink-equipped
`EventStore` as a `SessionEvent` is durable; everything else is not. The CLI wires the ROOT
store through `SessionManager` (`crates/norn-cli/src/runtime/from_cli.rs:171-180`);
`--no-session` uses a sink-less store by explicit choice.

---

## GAP 1 — Child agent stores (fork, spawn, rhai) have NO persistence sink. Child timelines are memory-only and lost. **CONFIRMED, data-loss (the big one)**

Evidence:
- Fork: `crates/norn/src/tools/agent/fork_pipeline.rs:220-226` — `resolve_fork_store` returns
  `Arc::new(EventStore::new())` (no sink) whenever no `SharedSessionTree` extension is present.
- Spawn: `crates/norn/src/tools/agent/spawn.rs:166-201` — `resolve_child_store`, same guard,
  same sink-less fallback (line 172).
- Rhai script children: `crates/norn/src/integration/rhai/agent_ops.rs:364` —
  `let child_store = EventStore::new();` (production; test module starts at 497).
- `SharedSessionTree` is **never published in production**: every
  `insert_extension(Arc::new(SharedSessionTree{..}))` site is inside `#[cfg(test)]`
  (`fork_tool.rs:1412` — tests start at 559; `spawn.rs:2569/4383/5368` — tests start at 651).
  No publisher exists in `agent/builder.rs`, `agent/assembly.rs`, or any of norn-cli/norn-tui.
  So the `else` branch is always taken.
- Even if the tree were published, it would not help: `SessionTree::branch` creates the child
  store as `Arc::new(EventStore::new())` (`crates/norn/src/session/tree.rs:215`) — no sink
  there either.

Mechanism / blast radius: everything a child appends to *its own* store evaporates at process
end — its AssistantMessages (per-call usage, thinking, reasoning items, stop reasons), its
ToolResults, its rule injections, its schedule lifecycle, its received `Delivered` audits, its
own `Sent` audits, its grandchildren's lifecycle Customs. What survives on the (root) parent's
disk: the spawn/fork tool-call arguments in the parent's AssistantMessage,
`subagent.started`/`subagent.completed` Customs (usage, subtree_usage, stop reason, no
content), the framed `<agent_result>` UserMessage when the parent's loop drains the result,
and `ForkComplete` for forks. Additionally, `ForkComplete.forked_session_id` falls back to the
fork's *agent registry UUID* when there is no SessionId (`fork_pipeline.rs:511-514`) — a
durable pointer to a session that exists nowhere on disk.

Minimal fix surface: `resolve_fork_store` / `resolve_child_store` / rhai `agent_ops.rs`, plus
a way to mint sink-equipped child sessions (SessionManager or a new branch primitive) and
record the parent↔child session linkage durably.

## GAP 2 — `SessionTree` is production dead code. **CONFIRMED**

`crates/norn/src/session/tree.rs` — in-memory tree of per-session `EventStore`s with
`branch()` (context-filtered seed + `Fork` event on parent) and `merge_summary()`. Its own
docs say "purely in-memory: there is no persistence" (tree.rs:10-11). The only
`SessionTree::new` callers outside `tree.rs` tests are in the `#[cfg(test)]` modules of
`fork_tool.rs`/`spawn.rs`. It is the intended branching seam and nothing constructs it.

## GAP 3 — No `Fork` branch-point event on the parent timeline for production forks/spawns. **CONFIRMED, attribution-loss**

`SessionEvent::Fork` is appended only by `SessionTree::branch` (`tree.rs:239-243`, dead per
Gap 2) and by `SessionManager::fork` (CLI `--fork` of a persisted session,
`manager.rs:361-368`). The live fork/spawn tools in standalone mode append no `Fork` event —
the standalone seeding path (`crates/norn/src/tools/agent/fork_seed.rs:25-140`) touches only
the child store. Parent-side provenance is only the `subagent.started` Custom (child_id,
descriptor, timestamp — no branch-point event id). A session-tree storage layer has no
durable anchor for where in the parent's timeline the branch occurred.

## GAP 4 — ActionLog / ActionLogTree / MutationLedger: in-memory; resume rebuild is lossy; child logs never persisted. **CONFIRMED, data-loss + attribution-loss**

- `crates/norn/src/session/action_log.rs:1-32` — the log is "an in-memory query layer over
  the session's EventStore".
- `crates/norn/src/session/action_log_tree.rs:33-38` — module docs state: "purely in-memory
  and session-scoped. On session resume, only the root agent's log is rebuilt … child session
  branches are not persisted today, so a resumed session's tree starts with the root alone."
  Federation across children exists live (registered parent→child at `spawn_context.rs:246` /
  `fork_pipeline.rs:196-202`) but none of it reaches disk except via the root store's
  ToolResult events.
- Rebuild limits, documented and real (`crates/norn/src/agent/resume.rs:13-28, 77-133`):
  follow-up actions (incl. `StoredContent` before-content) are closures, not persisted →
  resumed `write` to an existing file records as `Created` not `Modified` in the mutation
  ledger; entry timestamps become reconstruction-time; revert baselines re-hashed from file
  content at resume time (external edits absorbed); post-validate outcomes rebuild as `None`.
  Rebuild is wired for the root at `crates/norn/src/agent/assembly.rs:478`.

## GAP 5 — Persisted ToolResult stores the capped model-facing projection; oversized tool output is discarded from the durable log. **CONFIRMED, data-loss (bounded)**

`crates/norn/src/loop/tool_dispatch.rs:276-304` — `append_tool_result` runs
`model_safe_tool_output` (line 291) *before* the store append;
`crates/norn/src/tool/output_budget.rs:108-144` replaces an over-budget payload with
`{truncated_for_model, original_chars, head, tail, …}`. There is no spool of the full output
anywhere; the action_log Level-2 detail reads the same capped event back (`action_log.rs`
`get_detail`). The session file is the audit record and it holds a preview. Fix would
minimally touch `append_tool_result` (persist full, project at prompt-build time) or add a
spool reference.

## GAP 6 — Root step-level stop reasons (timeout / cancel / max-iterations) leave no record in the agent's own session log. **CONFIRMED, attribution-loss**

- Timeout: `crates/norn/src/loop/runner/entry.rs:283-294` — the timeout branch constructs
  `AgentStepResult::TimedOut` and returns; nothing is appended to the store marking the step
  timed out.
- Max iterations: `crates/norn/src/loop/runner/machine.rs:239-246` — returns
  `MaxIterationsReached` with no store event.
- Cancellation: `machine.rs:234` / `stop.rs` `BoundaryOutcome::Cancelled` — no store event.
- Contrast: truncation IS persisted (`loop.truncated` Custom,
  `crates/norn/src/loop/classify.rs:34-56`), and a *child's* abnormal stop reaches the
  *parent's* store via `subagent.completed` (`lifecycle.rs:161-174`). For the root agent
  there is no equivalent: a resumed root session cannot tell its previous step was cut off.
  Fix: append a `loop.*` Custom on the TimedOut/Cancelled/MaxIterations exits (timeout path
  must append after the inner future is dropped, next to `ensure_tool_results_complete` at
  `entry.rs:306`).

## GAP 7 — Mid-stream partial output of a hard-cut (timed-out/aborted) provider call is never persisted. **CONFIRMED, data-loss (partial content)**

`AssistantMessage` is appended only after a full response assembles
(`crates/norn/src/loop/runner/provider_call.rs:87, 95-176`). A step timeout is a hard cut
that drops the in-flight future (`entry.rs:126-139`); text/thinking deltas of the aborted
call were broadcast live-only. `timeout_state.last_assistant_text` holds only the last
*completed* turn's text (`provider_call.rs:72-74`). `ensure_tool_results_complete`
(`crates/norn/src/loop/helpers.rs:505-549`) repairs orphaned tool calls with synthetic
cancelled ToolResults but does not capture partial text.

## GAP 8 — Suppression and injection context-edit marks are not durable; resumed prompt view diverges. **CONFIRMED, resume-divergence**

`crates/norn/src/session/context_edit.rs:157-162` (doc on `apply_persisted_compactions`):
"Suppression and injection marks are live editing state and are not represented by durable
session events." Only compaction supersession is rebuilt on resume, from
`Compaction.replaced_event_ids` (`context_edit.rs:167-176`; wired once per loop context at
`crates/norn/src/loop/runner/setup.rs:95-100`). Any embedder/tool use of `suppress()`
produces a prompt view the resumed session cannot reproduce — suppressed events silently
reappear.

## GAP 9 — Compaction: solid core, one asymmetry between live and persisted records. **Mostly works; cosmetic gap**

Works: pre-compaction history preserved (append-only; supersession is marks-only),
`SessionEvent::Compaction` with summary + `replaced_event_ids` persisted
(`context_edit.rs:204-219` via `maybe_auto_compact`), `loop.compaction_summarization` Custom
persisted with summary_kind, error, model, freed_token_estimate, and the summarization call's
full usage (`crates/norn/src/loop/inflight_compaction.rs:216-237`), `loop.token_warning`
persisted (`inflight_compaction.rs:158-173`), marks rebuilt on resume. Gap: the live
`AgentEventKind::Compaction` (`AgentCompaction`, `inflight_compaction.rs:272-290`)
additionally carries `compaction_id`, `events_compacted`, `tokens_before`, `tokens_after` —
the persisted Custom carries none of these (no compaction_id correlation field;
`freed_token_estimate` is a different, pre-application estimate). A log-only consumer cannot
reproduce the live event's accounting exactly.

## GAP 10 — All secondary audit appends are best-effort: a sink failure leaves them non-fatal and non-durable while primary content continues. **CONFIRMED, data-loss under fault**

Logged-never-propagated append sites: `agent_message.delivered`
(`crates/norn/src/loop/delivery.rs:139-157` — deliberate, documented as
observability-gap-not-lost-message), subagent started/completed
(`crates/norn/src/tools/agent/lifecycle.rs:191-198`), `Sent` audit (`lifecycle.rs:44-71`),
`ForkComplete` (`fork_pipeline.rs:525-531`), queued-audit for re-queued inbound
(`delivery.rs:411-418`). Under a persistent sink fault the durable log keeps accepting
primary events on retry while the correlation layer thins out. Acceptable trade-offs
individually; collectively worth knowing for a storage-layer design that wants the audit
chain to be load-bearing.

## GAP 11 — `Sent` dual-store audit climbs exactly one level; grandchild message traffic never reaches disk. **CONFIRMED, subsumed by Gap 1 but distinct mechanism**

`crates/norn/src/tools/agent/coord/signal_agent.rs:532-534`: `Sent` is appended to the
sender's own store and to `grant.parent_store` (the immediate parent only). For a grandchild,
both stores are sink-less (Gap 1). Same pattern for the queued audit at
`signal_agent.rs:398-406`.

## GAP 12 — `/clear` swaps in a sink-less store on the slash state. **Latent inconsistency, currently near-harmless**

`crates/norn-cli/src/commands/slash/actions.rs:118-127` — `apply_clear_request` replaces the
`SlashState` store cell with `Arc::new(EventStore::new())` (no sink). The print
orchestrator's actual step runs against `parts.event_store` regardless
(`crates/norn-cli/src/print/orchestrator.rs:325`), and print mode is one-shot, so today this
mostly means `/clear` doesn't really clear the loop's store rather than losing data. The
TUI's `/new`/`/clear` does the correct rotation into a fresh sink-registered session
(`crates/norn-tui/src/app/rotation.rs:1-50`). Any future long-lived driver reading
`SlashState::current_store()` for subsequent steps would silently go memory-only.

## GAP 13 — Live-only event kinds with no persisted twin (by design, listed for completeness)

`AgentEventKind::UsageEstimate` and `AgentEventKind::StreamRetry`
(`crates/norn/src/provider/agent_event.rs:308-336, 508-527`) are broadcast-only telemetry.
`ToolCallDelta` (including its new `call_id` field) is stream-only, but nothing is lost: the
assembled call including `call_id` and `kind` is persisted on `AssistantMessage.tool_calls`
(`provider_call.rs:126-136`; `events.rs:72-92`), and the retry marker's effect (discard
partials) is invisible in the log because partials were never logged.

## GAP 14 — `Agent::run` returns a snapshot store without the sink when fork/spawn infra creates an Arc cycle. **CONFIRMED, embedder-facing caveat**

`crates/norn/src/agent/instance.rs:294` —
`Arc::try_unwrap(...).unwrap_or_else(|shared| snapshot_store(&shared))`;
`crates/norn/src/agent/assembly.rs:175-187` — "The persistence sink is not carried over".
Everything appended *during* the run was already written through; but the `RunOutcome` store
silently stops persisting for any appends an embedder makes afterwards, with no signal that
this happened.

---

## VERIFIED SOLID

- Root write-through durability: append persists before memory visibility; torn-line healing;
  duplicate-EventId tolerant reader; batched index with resume self-heal
  (`store.rs:441-470, 294-366`; `manager.rs:553-582`).
- `AssistantMessage` persists content, thinking, structured reasoning items (encrypted blobs
  for stateless replay), tool_calls with `call_id`/`kind`, per-call usage incl. `cost_usd`,
  `stop_reason`, `response_id` (`provider_call.rs:146-176`; `events.rs:123-164`).
- Injections (cron `norn:cron`, watch `norn:watch`, process-manager `norn:process-manager`,
  router traffic, embedder sends): framed `UserMessage` persisted byte-identical to what the
  model saw, followed by an `agent_message.delivered` Custom with
  `{message_id, from_id, from, to_id, seq, delivered_at}` — for every injected message,
  sequenced or not (`delivery.rs:87-185`); content-first ordering so no durable "delivered"
  claim without durable content; mid-batch failure preserves the remainder for the durable
  re-queue (`delivery.rs:360-420`); pending-message queued/dequeued audits + `from_events`
  rebuild (`pending_messages.rs`; `assembly.rs:764`).
- Subagent lifecycle `subagent.started`/`subagent.completed` Customs on the parent store with
  usage, `subtree_usage`, succeeded, error, typed stop (`lifecycle.rs:145-209`);
  `ForkComplete` with `result_summary` (the fork's structured/contract output, incl. partial
  output on timeout/truncation), usage, duration (`fork_pipeline.rs:296-321, 500-532`).
- Schedule lifecycle durable and rebuilt on resume: `schedule.created`/`fired`/`cancelled`
  with content-first ordering (`schedule/events.rs:1-60`; `schedule/store.rs:from_events`;
  `builder.rs:851`).
- Rule injections persisted as first-class `RuleInjection` events, including the step-timeout
  drop path (`events.rs:263-287`; `entry.rs:265-325`).
- Compaction core (see Gap 9), `loop.truncated`, `loop.token_warning` persisted;
  `ensure_tool_results_complete` closes orphaned tool calls on every exit path including
  external cancel (`helpers.rs:505-549`).
- Resume: full-file tolerant replay with `skipped_lines` surfaced; root action-log,
  pending-messages, schedule-store, and compaction-mark rebuilds all wired
  (`manager.rs:553-582`; `assembly.rs:478, 764`; `setup.rs:95-100`).
- Slash expansions: the raw user input is persisted even when a slash expansion replaces it
  in the prompt (`helpers.rs:66-71`; `setup.rs:102-131`).

## UNCERTAIN / NEEDS DEEPER TRACE

- Driven (jsonrpc) mode: whether one long-lived process serves multiple `run` requests
  against the same `parts.event_store`, making Gap 12's `/clear` divergence user-visible
  (`crates/norn-cli/src/print/orchestrator.rs`, `crates/norn-cli/src/print/driven.rs` not
  fully traced).
- TUI exit with queued-but-undelivered child results: the TUI consumes the result channel
  itself and queues framed prompts for the next turn boundary
  (`crates/norn-tui/src/app/child_results.rs:1-46`); `subagent.completed` (usage/stop, no
  content) is persisted regardless, but the result *content* is persisted only when injected
  as the next root prompt — exiting first likely loses it from the log. Not traced through
  the TUI exit path.
- Whether `record_dispatch_completion` (action log, `tool_dispatch.rs:213-219`) receives the
  full or capped output live (affects only live Level-2 queries pre-restart; the durable
  record is capped either way per Gap 5).
- `EventBase.parent_id` chaining is inconsistent: most loop appends chain
  `store.last_event_id()`, but e.g. `ContextEdits::summarize` appends Compaction with
  `parent_id: None` (`context_edit.rs:210-214`), and seeded child stores duplicate parent
  EventIds. If the planned session-tree layer wants to lean on parent links as a tree spine,
  they are not currently reliable.
