# Meridian Handoff — norn Phase 0+1 hardening + Phase 2 typed API

**Date:** 2026-06-12 (Phase 2 section added same day)
**Pin:** the current `main` head of `tomWhiting/norn` (≥ `2861545`, the
Phase 2 code commit; later commits to date are documentation-only).
**Status:** Phase 1 and Phase 2 each Fable-reviewed to READY across multiple
rounds, all findings fixed; gates green — clippy `--workspace -D warnings`
clean, fmt clean, **3083 tests / 0 failures**.

This document is for the agent working on the meridian/yggdrasil side. It
lists what changed in norn that affects you, what you must adapt when you bump
the pin, what you can delete, and which meridian-side fixes can proceed now
versus which must wait. Background: `REVIEW.md` (verified findings) and
`PLAN.md` (phased plan, owner decisions) in this repo.

**Read §7 first if you already read this doc before Phase 2 landed** — Phase 2
supersedes several Phase 1 rows below (they are marked). If you are bumping
the pin fresh, you get both phases at once; adapt to the Phase 2 surface
directly.

---

## 1. Update the pin

All four consumers (meridian-services, meridian-tools, meridian-aion,
meridian-vm-daemon) should move their git reference to the current `main`
head (≥ `2955a27`). The changes below are breaking; bump everything in one
pass.

## 2. Breaking API changes you must adapt to

| Old | New | Notes |
|---|---|---|
| `attach_sink(sink, dir, id)` (infallible) | **superseded by Phase 2** — `attach_sink` no longer exists; use `SessionManager` (§7.2) | Phase 1 briefly made it fallible with a `DurabilityPolicy` arg; Phase 2 replaced the whole constellation. |
| `read_session_events(..) -> Vec<Event>` | `-> SessionFileRead { events, skipped_lines }` | `skipped_lines` counts unparseable/torn/duplicate lines. Surface or log it — that's the point. |
| `SessionIndexEntry` | gains `format_version` | Match exhaustively / update constructors. |
| `from_auth_for_testing` | `from_static_auth` | Rename. **You currently call the old name in production code** — see §3, the right fix is deletion, not rename. |
| fork/spawn child results | non-`Completed` stop reasons report as failures; **Phase 2 adds the typed reason** — `ChildAgentResult.stop: Option<AgentStopReason>` + per-child `usage` | Branch on the typed stop, not strings. |
| `ProviderConfig.timeout` | now actually enforced | If you set absurd values "because it was ignored," fix them. |
| Provider errors | ~~`ProviderError::Truncated`~~ **superseded by Phase 2** — truncation is `RunOutcome::Stopped{reason: Truncated{kind}}` (§7.1), never an error and never retryable | The Phase 1 variant no longer exists. |
| Registry status transitions | typed `StatusTransitionError`; terminal states immutable | If you drive agent status, terminal→anything is now rejected. |

## 3. Code you can now DELETE on the meridian side

- **`NornSessionStore` (~540 LOC) and `reconcile_session_index`.** The session
  index is now self-maintaining: registered sinks batch index deltas and flush
  at `EventStore::checkpoint()` / drop; resume self-heals drifted entries;
  the reader is version-tolerant. Call `store.checkpoint()` at your turn
  boundaries (cheap; one locked index write) and delete your reconciliation.
- **`from_auth_for_testing` in production** — replace with real auth flow or
  `from_static_auth` where a static credential is genuinely intended.
- **Hand-rolled tolerant JSONL reading** — `read_session_events` is the
  tolerant reader now.

## 4. New capabilities you should adopt

- **`AgentBuilder::workspace_root(path)`** — real filesystem confinement for
  read/write/edit/patch/bash, **inherited by spawned/forked children** (children
  also snapshot the parent's working dir and inherit `PermissionPolicy`,
  `ToolEffectIndex`, and operator hooks). If meridian sandboxes agents, use
  this instead of convention.
- **`settings.permissions` is now enforced on the embedded path** (deny > ask >
  allow, shell-segmented matching). If you ship permission settings, they now
  actually do something — audit them before bumping, because rules that were
  silently dead will start blocking.
- **Headless reclamation:** `runtime_init::install_terminal_reclamation` is
  installed automatically by `AgentBuilder` assembly — naturally-completed
  child agents are reclaimed after result delivery instead of leaking
  registry entries/EventStores in long-running processes (this was a real
  leak for aion workers).
- **`rebuild_action_log` (now public) + `ActionLog::with_working_dir`** — if
  you resume sessions, rebuild the action ledger; meridian currently starts
  resumed sessions with an empty one.
- **LLM compaction summaries** — compaction now produces a provider-written
  summary (mechanical digest only as a marked fallback,
  `summary_kind: "mechanical_digest_fallback"`). No API change, but expect
  one extra provider call (usage-accounted) when compaction fires.
- **New provider knobs** (all optional): `rate_limit_interval` (default 60s),
  `retry_backoff` (default 1s), `retry_after_ceiling` (unset = honor server
  header as-is, saturated). Wired through settings and `-c` in the CLI;
  embedders set them on `ProviderConfig`.

## 5. Meridian-side fixes — go / wait

**GO NOW (independent of the pin):**
- **[critical] aion activities: check `stop_reason`** — stop reporting
  non-success as success (`activities/agent.rs:204-222`). Note: once you bump
  the pin, fork/spawn already report non-Completed as failure, but your
  activity-level check is still the authoritative guard.
- `futures::executor::block_on` in `dispatch` parking tokio workers.
- Nil-UUID caller identity for meridian tools in aion activities.
- No `cancel_token` / `step_timeout` on the aion path.
- Unbounded token-pool wait; activity `config` silently discarded on the norn
  route; non-idempotent `session_id` minting per retry attempt.
- God files (`assistant/service.rs` 1219, `norn_step.rs` 728, `catalog.rs`
  2139, `agent.rs` 575); hardcoded `gpt-5.5`; silent CWD-relative session
  fallback; unformatted `dispatcher.rs`.

**DO WITH THE PIN BUMP (same PR):**
- Session API call sites (§2 table), `NornSessionStore` deletion (§3),
  `checkpoint()` at turn boundaries, permission-settings audit (§4).

**~~WAIT FOR NORN PHASE 2~~ — PHASE 2 HAS LANDED.** Everything previously on
the wait list is now buildable; see §7. The retry-classification constraint is
resolved: build aion retry on `ErrorClass`/`RunOutcome` (§7.1) and delete the
string-prefix plan permanently.

## 6. Known gaps, recorded (don't rediscover these)

- Grandchild agents (spawned by spawned agents) have no result channel, so
  their registry entries persist until the parent child's handles drop —
  still open after Phase 2; recorded for Phase 3 (grandchild lifecycle
  *events* DO broadcast — only the result channel is missing).
- Hook envelopes receive an empty `ToolContext` (hooks can't read agent
  identity / working dir) — pre-existing, recorded as a future brief.
- `runner.rs` (~680 real LOC) is over the god-file cap by explicit owner
  deferral to the Phase 3 state-machine rebuild (see `PLAN.md`,
  review-round decisions).
- Bash confinement cannot stop `cd` escapes within a command's shell —
  documented limitation on `BashTool`; workspace_root confines the tool's
  declared paths, not arbitrary shell behavior.
- Hard-`NornError` child outcomes (and panicked child tasks) report
  `Usage::default()` — zeros mean "unknown", not "no tokens consumed";
  documented at every site. Early-stop outcomes carry real usage.
- Children do NOT inherit the parent's output schema — an explicit,
  documented decision (pass a schema per spawn if you want one), not an
  accident.

---

# 7. Phase 2 — typed API surface (landed at `2861545`)

Everything below is on `main` now. The Phase 2 design was driven by direct
inspection of your code — file references below are to yggdrasil as of
2026-06-12.

## 7.1 Run outcomes & retry (aion unblocked — build retry NOW on this)

```rust
let outcome: RunOutcome = agent.run(prompt).await?;   // #[must_use]
match outcome {
    RunOutcome::Completed(output) => { /* output.text(), output.usage() */ }
    RunOutcome::Stopped { reason, partial } => {
        // reason: AgentStopReason (serde-stable, snake_case) —
        // Truncated{kind} | TimedOut{..} | Cancelled | MaxIterationsReached
        // | SchemaUnreachable{..}. partial: AgentOutput with real usage +
        // the event store (resumable).
    }
}
```
- **A `Stopped` run is `Ok` and is NEVER a workflow success.** Record it as a
  non-success activity outcome with the typed reason. This replaces your
  `stop_reason` string check (`activities/agent.rs:204-222`).
- **Retry classification:** `err.class() -> ErrorClass` on
  `NornError`/`ProviderError` — `Retryable{kind}` / `RateLimited{retry_after:
  Option<Duration>}` (use the delay hint) / `Auth` / `Terminal`. Serde-able —
  it can cross your activity boundary. `is_retryable()` is the shorthand.
  Truncation can no longer appear as an error at all.
- OpenAI `response.incomplete` (max tokens / content filter) now produces the
  typed `Stopped{Truncated}` with partial text and usage — it was a hard
  error before Phase 2.

## 7.2 SessionManager (delete `NornSessionStore`; idempotent aion retries)

```rust
let mgr = SessionManager::new(data_dir);
// aion activity: deterministic key per (workflow, activity) — same key on
// retry resumes the SAME session instead of minting a new one:
let opened = mgr.open_or_resume(&key, CreateSessionOptions{model, working_dir, name}, DurabilityPolicy::Flush)?;
// opened.store (sink pre-registered), opened.entry (id, metadata),
// opened.replay.skipped_lines (log it)
```
- `create` / `resume` / `fork` / `list` / `resolve` / `read_events` /
  `rename` / `delete` cover the rest. `attach_sink`, `create_session`,
  `resume_session`, `fork_session` no longer exist.
- Session ids are validated (charset + reserved-name family: anything
  colliding with persistence-owned files like `index.*` is rejected,
  case-insensitively). Generate keys like `wf-{workflow_id}-{activity_id}`
  and you'll never hit it.
- Still call `store.checkpoint()` at turn boundaries.

## 7.3 AgentHandle (delete your 3 wiring copies; fixes `"model": ""`)

```rust
let mut builder = AgentBuilder::new(...)
    .event_channel_capacity(256)     // replaces hand-built broadcast + AgentEventSender
    .inbound_capacity(32);           // replaces hand-built inbound channel
let inbound = builder.inbound_sender();          // available PRE-build
let agent = builder.open_session(&mgr, SessionSpec::OpenOrResume{id: key}, DurabilityPolicy::Flush)?.build()?;
let handle = agent.handle();                     // Clone; take before running
let info = handle.info();   // resolved model/profile/tool_names/session_id/working_dir/output_schema
// aion cancellation (pattern is a compiled doctest in agent/handle.rs):
tokio::select! {
    outcome = agent.run(prompt) => { ... }
    () = activity_ctx.cancelled() => { handle.cancel(); /* then await the run: prompt Ok(Stopped{Cancelled}) */ }
}
```
- `info().model` kills the `"model": ""` emission at `activities/agent.rs:405`.
- Removed outright: `run_with`, `run_stream`, `.prompt()`, `.event_sender()`,
  `.inbound(rx)`, no-arg `run()`. One way to run: `agent.run(prompt)`.
- `norn::r#loop::…` → `norn::agent_loop::…` (mechanical sweep).
- No model and no profile = typed build error (delete your hardcoded
  `gpt-5.5` compensation and pass the model explicitly). Empty prompt =
  typed error.
- `ProcessEnv::new(pairs)` / `.merged(pairs)` replaces hand-built tuple
  structs (`dispatch_credentials.rs:103`, `norn_step.rs:129`).
- `AgentLoopConfig` is `Serialize`/`Deserialize` with `output_schema` on it —
  put it (or just the schema `Value`) in `NornActivityInput`; introspect via
  `info().output_schema`.

## 7.4 Typed subagent lifecycle (delete the `norn_translate.rs` parser)

Subscribe via `handle.subscribe()`; child events arrive on the same channel.
Lifecycle events are typed (`SubagentLifecycle::Started/Completed`, internally
tagged `phase`, snake_case) carrying `parent_id`, `child_id`, descriptor
(`kind: spawn|fork`, role, model, profile), `started_at`/`completed_at`
(RFC 3339), per-child `usage`, `succeeded`, `error`, and typed `stop`
(AgentStopReason). The same payloads are appended to the parent's session
store as `Custom` audit events (`subagent.started` / `subagent.completed`
event types) for replay.
- Replaces the `output["agent_id"] | output["fork_id"]` + `status != "active"`
  scraping in `norn_translate.rs:200-230`. Spawn and fork tool outputs now
  both say `agent_id`.
- A panicking child still emits `Completed{succeeded: false}` — no dangling
  `Started`.

## 7.5 Typed tool errors (dispatch on kind, not prose)

Every tool failure — soft, hard, permission-denied, hook-blocked — persists as
`{"error": {kind, message, detail}}` in the ToolResult event, and
`SingleToolResult.error: Option<ToolErrorPayload>` carries it typed in-process.
Kinds: `invalid_arguments | missing_extension | not_found | blocked |
validation_failed | permission_denied | conflict | timeout | io | network |
external_service | execution_failed | <custom>`. Permission blocks carry
`{rule, decision, reason}` in detail; hook blocks `{hook, reason}`.

## 7.6 Tool definition (delete `catalog.rs` — 2,139 lines)

- Derive: `#[derive(Deserialize, ToolArgs)]` on a `#[serde(tag = "command")]`
  enum — doc comments become schema + catalog descriptions, field types/`Option`
  become hints/required flags, nested `ToolArgs` types compose.
- Implement `CompositeTool` (typed `Command`, `command_effect` per variant —
  adding a command without classifying its effect is a compile error,
  `conservative_effect` as the ≥-join; apply
  `assert_conservative_effect_covers_all_commands` in a test) and the blanket
  impl gives you the `Tool` surface, per-command catalog entries, and
  effect-aware scheduling. `TaskTool` in-tree is the reference conversion.
- `ToolEffect::RemoteMutation` is the honest effect for DB/Redis mutations
  (serialized like `Write`) — delete the disk-`Write`-by-convention comments.
- `ctx.require_extension::<T>()` replaces the get+ok_or boilerplate.

## 7.7 Suggested adoption order

1. Bump pin; mechanical sweep (`agent_loop`, `run(prompt)`, `RunOutcome`
   match, `ToolOutput` constructor changes if you build any by hand).
2. aion correctness batch: `RunOutcome` recording + `ErrorClass` retry +
   `open_or_resume` idempotent sessions + `handle.cancel()` wiring — this
   clears four of your critical/high items in one pass.
3. Delete: `NornSessionStore` remnants, the 3 wiring copies, `norn_translate`
   subagent parser, `catalog.rs` (convert tools to `CompositeTool` +
   `ToolArgs` derive as you go).
4. The rest of the go-now list (block_on, nil-UUID, token-pool bounds, god
   files) — unchanged, still yours, still independent.

---

# 8. Smoke-test hardening batch (landed at `514a553`) + Wave 3 pre-announcement

A production tool smoke test surfaced provider-schema and agent-lifecycle
defects; the fix batch is on main at `514a553` (Fable-reviewed, 3,139
tests green). Impact on meridian, smallest first:

## 8.1 Landed at `514a553` — adapt when you bump past it

- **`norn::provider::resolve_provider_tools` is DELETED** (no shim). The
  replacement is `norn::provider::ResolvedToolSurface::resolve(&defs,
  caps).provider_definitions()`, or `collect_function_definitions(registry,
  allow_list)` for the registry→definitions projection. If you never called
  it directly (the agent loop resolves per-request internally), nothing to
  do.
- **Composite tool arguments are now strictly validated** against the
  canonical per-command schema after deserialization: unknown fields are a
  typed `invalid_arguments` soft failure naming the field and the command's
  accepted signature, instead of being silently dropped. If any meridian
  code constructs composite-tool args programmatically (task tool included),
  stray fields that used to be ignored now fail loudly — that is the point;
  fix the call sites, don't pad the schemas.
- **`close_agent` semantics changed.** It now cancels the child's actual
  run (CancellationToken plumbed into the loop; in-flight provider call
  interrupted immediately, executing tools finish first) and joins the
  completion wrapper so the run's REAL outcome is recorded — a mid-run
  close lands `Failed` + `AgentStopReason::Cancelled`, never a falsified
  `Completed`. `shut_down` entries now carry one of: `reclaimed`,
  `already_completed`, `force_failed`, `unreachable`, `failed`, `missing`.
  If meridian parses close results, match on these.
- **Registry tombstones.** Reclaimed agents leave an `AgentTombstone`
  (re-exported from `norn::agent`: id, path, terminal status,
  `completed_at`). `signal_agent` to a finished agent is a structured
  delivery failure carrying `recipient_status`/`completed_at`; "not
  registered" now only ever means the id never existed. `AgentEntry`
  gained `completed_at: Option<DateTime<Utc>>` — if you construct entries
  in tests, add the field.
- **web_search is provider-aware end-to-end.** On providers reporting
  `hosted_web_search` (OpenAI today, Anthropic when added) the function
  tool is replaced by the platform's hosted tool and the catalog/system
  prompt say so; elsewhere the function tool serves. Nothing to adapt
  unless you hardcoded assumptions about `web_search` being a function.

Serde-stable surfaces from §7 (`RunOutcome`, `ErrorClass`,
`AgentStopReason`, `SubagentLifecycle`, `ToolErrorPayload`) are
**unchanged** in this batch. Wave 2 has since LANDED at `8e10b9d`
(Fable-reviewed, 3,191 tests): every agent — root, fork, spawn — now
has its own `ActionLog` in a session-wide `ActionLogTree`; the
`action_log` tool gained an optional `scope` (absent = exact prior
behavior, so existing callers are unaffected); a read-only `agents`
status tool (list/get over registry + tombstones) joined the standard
set, and `AgentTombstone` gained `parent_id`. All additive — the only
adaptation: if meridian assembles registries by hand and enumerates the
standard set, the set now includes `agents`. Pin guidance: current main
head (`8e10b9d` or later).

## 8.2 Wave 3 pre-announcement — ONE breaking schema change is coming

Approved 2026-06-12 (design: `docs/design/agent-coordination-wave3.md`):
inter-agent messaging (`send_message` REPLACES `signal_agent` outright)
and recursive delegation (children may spawn children under
parent-configured budgets).

- **BREAKING: `SubagentLifecycle::Completed` gains `subtree_usage`**
  (aggregated descendant usage) — coordinate your match/deserialization
  update with the pin bump when Wave 3 lands; we will flag the exact
  commit in this document.
- **BREAKING: `signal_agent` will be deleted** in the same wave, replaced
  by `send_message` (target path/UUID/"parent", kind `steer`/`update`,
  scope enforced from spawn-time policy). **SUPERSEDED — the final name
  is `signal_agent` after all** (owner decision: meridian's own
  workspace member-messaging tool collides with a `send_message` name).
  The tool was briefly `send_message` between the W3.2 and rename
  commits, never in a release you should pin. Net effect for meridian:
  NO tool rename — keep `signal_agent`, adapt only to the new args and
  semantics (§8.4).
- New builder requirements when agent-coordination tools are enabled:
  `child_policy` envelope (messaging scope, delegation budget, channel
  capacities) becomes builder-required — build error if spawn tools are
  registered without it. Documented proposals match today's behavior
  (depth 1, 32 children, capacities 32/256).
- Tracked deferral you should know about: children do NOT inherit the
  parent's `AgentLoopConfig` (`max_iterations`/`step_timeout`) and won't
  in Wave 3 either; per-child loop config is deferred to the following
  wave and recorded in the design doc.

## 8.3 Wave 3 batch 1 landed (W3.0 + W3.1 + W3.3) — adapt when you bump past it

Two-round Fable review, gates 3245/0. Adaptations if you pin past this
commit (the `subtree_usage` schema break is **not** in this batch — it
arrives with W3.6 as pre-announced above):

- **BREAKING (build-time): the coordination envelope is now required.**
  `AgentBuilder` builds that wire `.agent_registry(..)` must also call
  `.child_policy(ChildPolicy { messaging, delegation, inbound_capacity })`
  and `.child_result_capacity(n)` or `build()` returns a typed
  `ConfigError::InvalidConfig` naming the missing setters. Recommended
  starting envelope (documented proposals, matching previous behavior):
  `MessagingScope::SiblingsAndParent`, `DelegationBudget { remaining_depth:
  1, max_concurrent_children: 32 }`, `inbound_capacity: 32`,
  `child_result_capacity: 256`. Setting the envelope WITHOUT a registry is
  also a build error. The library const `CHILD_RESULT_CHANNEL_CAPACITY` is
  deleted — the channel is sized from your envelope.
- **BREAKING (API): `agent::Mailbox` is deleted**, replaced by
  `agent::MessageRouter` (typed `RouteError::{NotRouted, ChannelClosed,
  ChannelFull}`; per-recipient seq minted at enqueue). If you constructed
  `AgentToolInfra` directly, its `mailbox` field is now `router:
  Arc<MessageRouter>`. `AgentError::MailboxClosed` is deleted.
- **BREAKING (API): `ChannelMessage` reshaped** — `author`/`DeliveryMode`
  are gone; fields are `{id, sender_id, from, role, to_id, content, kind:
  MessageKind::{Steer,Update}, seq, timestamp}`. Inbound injection renders
  as an escaped `<agent_message from= from_id= [role=] kind= [seq=] ts=>`
  frame (the old `[Inbound from {author}]` format was sender-forgeable);
  child results render as `<agent_result from= from_id= succeeded=>`
  frames via the new `agent::frame_child_result` (the old `[Agent result
  from …]` raw injection could forge frames). If meridian parses either
  injected format out of stored `UserMessage` events, parse the frames.
- **`AgentEventKind` gained a third variant `Message(AgentMessageLifecycle)`**
  — exhaustive matches need an arm. New store audit events (`Custom`):
  `agent_message.sent` / `agent_message.delivered` (serde shape-pinned;
  resume-safe — replay ignores Custom events).
- **`signal_agent` result payload changed** (this batch only; the tool is
  deleted in W3.2): success now `{delivered, to, kind, seq, message_id,
  routed_via, trigger_turn}`; new typed failures for closed/full routes.
- **Opt-in linger**: `AgentLoopConfig.linger: Option<LingerPolicy {
  deadline }>` — absent field deserializes to `None` (legacy configs
  unchanged); unset preserves return-immediately behavior byte-identically.
  A lingering parent waits at stop boundaries for late child results and
  steer messages (steer wakes it; update does not).

## 8.4 W3.2 landed — the messaging tool replaces old `signal_agent` (final name: `signal_agent`)

Adaptations when you bump past the W3.2 + rename commits (see the
superseded note in §8.2 — the tool keeps the `signal_agent` name; it was
briefly `send_message` in between, never in anything you should pin):

- **BREAKING (tool surface): old `signal_agent` semantics are replaced
  in place.** Same name, new contract: args `{to: <path | UUID |
  "parent">, kind: "steer"|"update", content: string}`
  (additionalProperties: false).
  Success payload: `{delivered, to, kind, seq, message_id}`. Failures are
  typed and honest: unknown identifier, already-finished recipient (with
  recorded status + completion time), out-of-scope (PermissionDenied naming
  the granted scope), no delivery route, closed channel. No tool rename
  needed on the meridian side; the renderer/translator layers that read
  the old `agent_path`/`message` args must read `to`/`kind`/`content`.
- **Messaging scope is enforced**: a child may message per its granted
  `ChildPolicy.messaging` (`siblings_and_parent` | `parent_only` | `none`;
  `none` also strips the tool from the child's surface). A root agent may
  message only its own children. Escalation is one audited hop at a time.
  (All `send_message` references in earlier drafts of this section refer
  to today's `signal_agent`.)
- **BREAKING (API, if you construct `AgentToolInfra` by hand):** the
  `policy`/`parent_store` fields are one bundled `grant:
  Option<ParentGrant { policy, parent_store }>` — `Some` for spawn/fork
  children (stamped by the launch paths), `None` for roots.
- **Dual-store audit**: every accepted send appends `agent_message.sent`
  to the sender's store AND the scope-granting parent's store; delivery
  appends `agent_message.delivered` in the recipient's store. A `Sent`
  without a paired `Delivered` means the recipient's loop ended before
  draining it.
- **Child inbound capacity** now comes from `ChildPolicy.inbound_capacity`
  (the hardcoded 32-buffer consts are deleted); root child-result channels
  are sized from the builder envelope everywhere (the CLI's local 256
  consts are gone too).
- **Known state on CLI-style surfaces**: a root built without
  `inbound_capacity` cannot receive `signal_agent(to: "parent")` — children
  get the precise typed failure ("the root agent has no inbound channel
  configured"). If meridian grants children `siblings_and_parent` or
  `parent_only` and wants child→parent messaging to work, set
  `AgentBuilder::inbound_capacity` on the root and drain the channel.
  Tracked follow-up for norn's own CLI drivers.

## 8.5 W3.4 landed — delegation budgets + recursion functional

Children can now spawn children, governed by harness-stamped budgets.
Adaptations when you bump past the W3.4 commit:

- **BREAKING (API): `AgentRegistry::reserve`** gains two parameters —
  the child's stamped `policy: ChildPolicy` and
  `unregistered_spawner_policy: Option<&ChildPolicy>` (envelope fallback
  for unregistered roots). Every embedder call site breaks. Budgets are
  enforced under the registry write lock against the SPAWNING agent's
  own granted policy; refusals are typed and name the budget. A
  terminal spawner can no longer reserve children.
- **BREAKING (API): `AgentEntry`** gains a required `policy` field
  (struct construction + strict deserialization). The type is not
  persisted by norn (in-memory registry only) — no resume break — but
  if meridian constructs entries or round-trips the struct, adapt.
- **BREAKING (API): `NornRhaiContext`** gains a required
  `child_policy: ChildPolicy` (the script host's own granted policy,
  embedder-supplied — never defaulted).
- **SEMANTIC — read this one carefully: `CoordinationEnvelope.child_policy`
  is the root's OWN policy**, the budget its spawns are charged
  against. Children receive a derived grant with `remaining_depth`
  decremented one level (optionally narrowed further by the new
  per-spawn `child_policy` arg on spawn_agent/fork). At the documented
  proposal (`remaining_depth = 1`) behavior is identical to W3.2
  (children are leaves); at deeper values children get N−1, not N.
- **Path shapes**: auto-generated child paths now nest under the
  spawner — on the CLI surface children move from `/spawn/{uuid}` to
  `/root/spawn/{uuid}`, grandchildren to `/root/spawn/{a}/spawn/{b}`.
  Anything parsing registry paths sees the new shape; `parent_id` links
  remain the authoritative genealogy. Explicit `path` args are
  untouched.
- **`agents` tool output**: live entries gain a read-only `policy`
  object (additive); tombstones do not retain it.
- **§6 gap CLOSED**: grandchild registry entries no longer leak —
  per-agent result channels exist at every delegating level and
  delivery-anchored reclamation runs at every level (pinned by depth-2
  tests on both spawn and fork surfaces).
- **Spawn/fork args are now strict**: unknown top-level keys are typed
  failures (a typo'd `child_policy` can no longer silently drop a
  narrowing).
- R5 still stands (and is more visible at depth): mid-tree agents
  cannot linger, so a delegating child that returns before its own
  children finish loses their results (error-logged; stated in the
  spawn/fork guidance). Closes next wave with `ChildPolicy.loop_config`.

## 8.6 W3.5 landed — cancellation cascades through the agent tree

Adapt when you bump past the W3.5 commit. Two-round-equivalent Fable
review (READY; one test gap closed pre-commit).

- **New: `AgentCancellation(pub CancellationToken)`** (exported from
  `norn::tools::agent`), a ToolContext extension published on the shared
  context. **This deviates from the design doc's letter** (which put the
  token on `AgentToolInfra`) — recorded as appendix item 9 in the Wave 3
  design doc: embedder assemblies that own no run token must not be
  forced to invent one.
- **Library surface**: `AgentBuilder::build` resolves the run token once
  (your explicit `.cancel_token(..)` or a fresh one) and uses that single
  token for `Agent`/`AgentHandle` *and* the published extension —
  **cancelling the root handle now ends the whole spawned subtree**.
  Spawn/fork mint child tokens via `child_token()` of the published
  extension at every depth; results bubble as honest `Cancelled`
  outcomes with usage, and full-subtree reclamation is pinned by depth-2
  tests.
- **Embedder boundary (meridian, read carefully)**: if your assembly
  constructs `AgentToolInfra` directly and you hold a root run token,
  publish `Arc::new(AgentCancellation(your_root_token))` on the shared
  tool context to opt your tree into the cascade. If you publish
  nothing, direct children get free-standing tokens (exactly pre-W3.5
  behavior, `close_agent` still works per-agent) — but any root-level
  cancel you implement yourself will NOT reach them. Named follow-up in
  the design doc.
- **`close_agent` output (additive)**: live descendants under a
  triggered cascade now report status `"cancelling"`; `"unreachable"`
  survives only for genuinely broken lineage. The close fires the
  target's token before its leaves-first walk and returns only after the
  *target's* wrapper completes.
- rhai script children remain `cancel: None` (no host token exists to
  parent under); documented at the executor construction site.

## 8.7 Structured-output envelope fix + reserved-key contract

Lands with the W3.5 commit. Fixes a real headless failure:
`[schema-violation] ... ('tool_use_description', 'tool_use_metadata'
were unexpected)` whenever an output schema set
`additionalProperties: false`.

- The structured-output tool's model-facing definition is now
  envelope-wrapped like every other tool, and the envelope fields are
  split off the model's arguments **before** schema validation. Your
  output schemas keep their `additionalProperties: false`; the validated
  output never contains the envelope keys; the raw call (description
  included) stays on the persisted `AssistantMessage` event.
- **NEW CONTRACT (breaking only if you collide)**: an output schema may
  not declare top-level properties named `tool_use_description` or
  `tool_use_metadata` — those names are reserved by the tool-call
  envelope across the whole tool surface. Colliding schemas are refused
  with a typed `SchemaError::InvalidSchema` at the `spawn_agent`
  argument boundary (synchronous) and at agent-loop entry (backstop for
  embedder/fork/rhai schemas). Previously such schemas "worked" by
  silently losing data; rename the property if you have one.
- Map-shaped output schemas (top-level `additionalProperties`, no
  `properties`) pass the check, but the two reserved names remain
  claimed at the top level: a data entry under either name is stripped
  before validation. Documented on `wrap_schema_with_envelope`.
