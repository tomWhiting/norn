# Norn — Plan to Ideal State (2026-06-11)

Companion to `REVIEW.md` (verified findings, file:line detail). This plan is driven by how norn is *actually consumed*:

## Usage profile (measured, not assumed)

Norn is embedded **as a library** everywhere that matters. Nobody in production drives it through norn-cli.

| Consumer | Path | What it uses |
|---|---|---|
| meridian-services assistant sessions | `AgentBuilder` + `load_runtime_base` | sessions (own store!), inbound channel (Steer), AgentEvent broadcast, notification injector hook, registrar-injected meridian tools, spawn/fork consumed via untyped JSON |
| meridian-services rhai workflow steps | `AgentBuilder` + `load_runtime_base` | norn's own session persistence + **hand-rolled index reconciliation**, `step_timeout`, `output_schema`, `cancel_token`, LSP backend/workspace pooling |
| meridian gleam/BEAM bridge steps | bare `AgentBuilder` | no runtime base, no sessions, no events, no cancellation |
| **meridian-aion durable activities** (active dev) | `AgentBuilder` + `load_runtime_base` | sessions (meridian's tolerant store), event forwarding; **no cancel_token, no step_timeout, no output_schema, no retry classification** |
| meridian-tools (8 composite tools) | `Tool` trait | five-phase lifecycle (only execute + follow_ups used), honest effects **that norn never consumes**, ToolContext extensions, `ToolArgs` derive |
| meridian-vm-daemon | openai_oauth types | credential seal/unseal reaching into `AuthDotJson` internals; `from_auth_for_testing` in the production credential path |

Concurrency reality: many agents per process, multiple norn-embedding processes per machine (server + vm-daemon) sharing `~/.norn` — the cross-process session-index race is a live production risk, not theoretical.

Key facts that reshape priorities vs REVIEW.md:
- The **embedded builder path** (H13/H14 hook bugs, builder gaps) is the production path. The CLI stream-json hang (C1) matters for CLI users only.
- Meridian **already re-implemented** a corruption-tolerant session reader because norn's is fail-fast (H19 confirmed by a consumer working around it), and hand-maintains the session index because agents don't (H21's root cause).
- `ProviderConfig.timeout` no-op (H4): meridian sets 2min in `workflow/provider.rs:79` — silently ignored in production. In aion activities there is **no other timeout**: a stalled stream = permanently hung activity *and* a parked tokio worker.
- OAuth single-flight (H6) and the 429 limiter inversion (H5) are directly exposed by meridian's many-concurrent-agents-shared-provider topology.
- The effect system meridian-tools faithfully implements is **never consumed** — `SchedulingPlan::build` and `effect_for_args` have zero callers in dispatch (verified). Tools are paying a contract cost for a scheduler that doesn't run.
- MCP (H7) is **unused** by meridian — deprioritized accordingly.
- aion needs from norn: a run API that can't silently treat `MaxIterations/Cancelled/SchemaUnreachable/TimedOut` as success, retryable-vs-terminal error taxonomy, usage-on-failure, serializable structured-output plumbing.

---

## Phase 0 — Make the stated gates true (small, do first)

The CLAUDE.md promises must hold before anything else lands on top.

1. **Fix the 2 clippy `too_many_arguments` errors** (lib-test target) by restructuring signatures (parameter structs), not `#[allow]`. Gate: `cargo clippy --workspace --all-targets -- -D warnings` clean.
2. **Fix the 5 failing tests** (full diagnosis in REVIEW.md):
   - `lsp_path_used_outcome_skips_server_and_inline_paths`: real bug — restore D4 cascade gating in `diagnostics_check/post_check.rs` (run `try_lsp_diagnostics_for_rules` first; skip `ToolRef::Diagnostic` dispatch on `Used`).
   - 4× `server_path_*`: test rot — register a stub `EchoDiagnosticAdapter` in test infra; keep the hardened missing-adapter Block behavior.
3. **Delete dead weight**: the stray tracked file `crates/norn-tui/src/error.rs:7:1`; the dead `tools/patch_output.rs`; the 3 `allow(dead_code)` sites; the one production `allow(clippy::unwrap_used)` in `patch_entity.rs:93`.
4. **Resolve the standards self-contradiction**: CLAUDE.md's Linting section bans all `#[allow]` while Error Handling permits test-module unwraps. Decide policy (recommend: allows permitted *only* on `#[cfg(test)]` items, stated explicitly) and codify it in CLAUDE.md.

Exit: clippy/fmt/test gates all green; CI enforcing them.

## Phase 1 — Verified correctness fixes, ordered by consumer exposure

All file:line and failure scenarios are in REVIEW.md. Order within phase = exposure for meridian/aion.

**1a. Provider & auth (every meridian agent, every turn)**
- Apply `ProviderConfig.timeout`: connect timeout + SSE inactivity timeout (per-chunk deadline), honoring the configured value end-to-end (H4).
- Fix the 429 handler: back off `permits_per_interval`/interval in the correct direction, with decay back to baseline; parse HTTP-date `Retry-After` (H5).
- Single-flight OAuth refresh (hold guard across the exchange or dedup in-flight refreshes); atomic `auth.json` write (temp+rename) since the file is shared with the Codex CLI (H6 + mediums).
- Promote `from_auth_for_testing` to a real constructor: `AuthManager::from_static_auth(..)` — meridian's VM credential path ships it today.
- Don't discard a successful refresh when persisting fails; surface partial usage on failed runs (aion gap #3).

**1b. Loop state machine (every embedded agent using schemas/compaction/resume)**
- `ToolsAndSchemaValid`: stop double-appending the schema tool result (H1).
- `SchemaInvalid`: reject/answer post-schema tool calls like the valid arm does (H3).
- Developer-message sync: track the managed Developer message by ID, never by first-role match; stop destroying compaction summaries on resume (H2).
- While here (mediums worth bundling): wire `latest_errors` so `RepeatedFailure` can fire; surface MaxTokens/ContentFilter truncation as non-success; make auto-compaction produce an actual summary and apply within the triggering step.

**1c. Embedded builder wiring (the path meridian actually runs)**
- Fix `assemble_hook_registry` merge: merge without the `Arc::try_unwrap` trap; never silently drop either hook source; one implementation, loud on conflict (H13).
- Publish `HookRegistry` on the ToolContext in the builder path so subagent hooks fire (H14).
- `agent_registry()`: wire AgentHandles/ChildResultSender/child_result_rx completely or fail the build with a clear error (medium, confirmed).
- Don't silently replace a caller-supplied `DiagnosticCollector`.
- `signal_agent`: remove the mailbox fallback or make mailboxes actually shared/drained; never report success into a void (H15).
- Fork/spawn outcome mapping: `MaxIterations/SchemaUnreachable/TimedOut/Cancelled` children are not `Completed` (medium; same bug class aion hit at the activity level).

**1d. Session durability (meridian runs multi-process; they already built workarounds)**
- **Upstream the tolerant reader**: skip-with-warning on torn/unknown lines, hard-fail only on structural corruption; add JSONL version header + writer version in index (REVIEW R4). Deletes meridian's `NornSessionStore` raison d'être.
- `attach_sink` and sink `persist()` return errors; no silent in-memory degradation (failure-modes medium + meridian PART A2).
- Inter-process index safety: advisory `flock` around read-modify-rewrite, or move per-session metadata into the session file and rebuild index lazily (H18).
- **Agents maintain their own index** (event counts, usage, updated_at on append) so embedders stop hand-reconciling (kills meridian's `reconcile_session_index`, fixes H21's class).
- Rebuild ActionLog/MutationLedger on resume per the documented contract; hash ledger paths against the agent working dir (mediums).
- fsync policy: make live-path durability explicit and configurable (default documented).

**1e. Tool execution safety (workflow agents edit code and run commands)**
- Bash: process-group spawn + killpg on timeout; bounded drain after exit (don't await pipe EOF forever) (H12).
- Patch: merge multiple blocks per file sequentially (H8); occurrence disambiguation using the `@@` line with ambiguity refusal like EditTool (H9); fix CC interleaved-context parsing and use the `@@` anchor (H10); preserve original line endings + trailing-newline state (high).
- Atomic file commits everywhere (temp+rename) for edit/write/patch (H11).
- Workspace confinement policy: opt-in root + symlink containment for read/write/edit/patch; `working_dir` from the model validated against it (medium, security).
- web_fetch SSRF guard (block loopback/link-local/RFC1918 by default, configurable) and first-token risk classification fix (mediums, security).

**1f. Honesty items — enforce or delete (CLAUDE.md: no silent failures)**
- `permissions.allow/deny/ask`: enforce in tool dispatch or remove the config surface entirely (H16).
- `--disallowed-tools`: gate the registry or hard-error unimplemented (H17).
- **Wire `SchedulingPlan` into `tool_dispatch`** so declared effects actually schedule concurrent ReadOnly/Network batches — or explicitly demote effects to documentation. Recommend wiring: meridian-tools already did the hard part (verified honest), and it's the designed payoff.
- Fix the CLI stream-json hang (C1) — store a `Weak` sender or remove the context-held clone before awaiting the renderer.

## Phase 2 — API reshaping for embedders (breaking changes welcome per CLAUDE.md)

Each item traces to a measured consumer pain point.

1. **Typed run outcome.** `run_with` returning `Ok` for `MaxIterations/Cancelled/TimedOut/SchemaUnreachable` caused aion to durably record failures as successes (their critical #1). Split: `run()` → `RunOutcome { Completed(AgentOutput) | Stopped { reason, partial: AgentOutput } }` (or `AgentOutput::into_result()` + `#[must_use]`), with usage populated on *all* arms.
2. **Error taxonomy.** `NornError`/`ProviderError` gain `is_retryable()` / `ErrorClass::{Retryable, Terminal, Timeout, Cancelled}` — aion's engine contract needs exactly this; the loop's own retry classifier already has the logic, expose it.
3. **`AgentHandle` run bundle.** One constructed handle exposing: event subscription (broadcast *and* a backpressured mpsc sink option for durable persistence), cancel token, inbound sender (without the rx/tx ownership split meridian models around), and **introspection** (resolved model/profile/tool inventory/context window — meridian emits `model: ""` today because nothing exposes this). Collapses the 3 near-identical ~60-line wiring copies in meridian.
4. **`SessionManager`.** One API owning create/resume/fork/attach/index/data-dir: `SessionManager::open(dir, mode)`; no `Option<PathBuf>` home-guessing (explicit dir, library provides `default_data_dir()`); tolerant versioned reader from 1d. Meridian deletes `NornSessionStore` (540 LOC) and both call-site fallbacks.
5. **Tool API ergonomics** (every item is repeated boilerplate measured in meridian-tools):
   - `ToolContext::require_extension::<T>() -> Result<Arc<T>, ToolError>` with a standard model-facing error (8 verbatim copies today).
   - Registry stamps `ToolOutput.duration` (8 copies of `Instant::now()`).
   - `CompositeTool`: declare subcommands with per-command effect + handler; derive `effect()`, `effect_for_args()`, schema enum, invalid-command error (prevents the member.rs drift class).
   - Typed error payload on `ToolOutput` (`{code, message, detail}`) replacing the bare `is_error` bool (meridian invented this shape; follow-up predicates string-match JSON today).
   - `PreValidateOutcome::Block` carries structured content so validation can move out of `execute`.
   - `ToolArgs` derive emits `ToolFieldHint`s → catalog entries derived from schemas (deletes meridian's 2,139-line hand table).
   - Effect vocabulary: add a remote-state effect (meridian maps DB/Redis mutations onto disk-`Write` by convention, with "norn has no System variant" comments).
6. **Typed subagent lifecycle events.** `AgentEvent::Subagent{Started,Completed,...}` with structured payloads — meridian currently reverse-engineers `output["agent_id"] | output["fork_id"]` and `status != "active"` from untyped JSON.
7. **Builder hygiene.** `ProcessEnv` constructor/merge API (two call sites build the raw tuple-struct); rename `r#loop` → `agent_loop` (every consumer escapes it); explicit policy when neither profile nor model is set (hard error or exposed default — meridian hardcodes `gpt-5.5` to mirror norn-cli today); guard or define empty-prompt runs (meridian can issue one today).
8. **Structured output for serialized boundaries.** Schema plumbing serializable into an activity/step input (aion's input struct has no schema field partly because there's no obvious serialized form).

## Phase 3 — Structure, standards, strategy

1. **R1 — One runtime assembly** (REVIEW): collapse library/CLI/TUI copies onto `runtime_init`; CLI builds via `AgentBuilder`. Deletes ~600–800 LOC and the drift class that produced H13/H14/H20.
2. **R2 — Runner state machine**: `StepState` struct + named phase methods; in-file tests move out. Also clears `runner.rs` (673) and `agent/builder.rs` (754) from the god-file list.
3. **God files & mod.rs purity**: split remaining >500-LOC files (`patch_apply.rs` 647, `config/merge.rs` 559, `patch.rs` 554, `runtime_init.rs` 535); evacuate logic from `provider/openai/mod.rs` (33 fns), `tools/search/mod.rs` (31), `tools/bash/mod.rs` (14), `tools/follow_up/mod.rs` (9).
4. **R3 — Provider-neutral request + native Anthropic provider**: extract `ConversationThreading`, capability-gate OpenAI-isms; the second native backend is the test of the seam.
5. **Observability**: span-per-iteration and span-per-tool-call (agent_id/session_id/tokens/duration fields) — meridian/aion correlate across process boundaries today with hand-built event bridges; spans are the cheap unification.
6. **SSE conformance fixtures**: recorded wire fixtures for the 1,286-line hand-rolled parser; cross-provider contract test once R3 lands.
7. **MCP correctness** (deprioritized — unused by meridian): id-correlated responses, timeout-safe transport (H7) — do before any consumer adopts MCP.

## Meridian-side fixes (not norn's to fix — flag to the aion/meridian work, which is under active development)

- **[critical] aion activities report non-success stop reasons as success** (`activities/agent.rs:204-222`) — check `stop_reason` now; Phase 2.1 makes this impossible to miss later.
- **[high] `futures::executor::block_on` in `dispatch`** parks a tokio worker per agent run (engine only calls the sync path) — N concurrent agents ≥ worker count deadlocks the engine.
- **[high] Nil-UUID caller identity** for meridian tools in aion activities — real attribution/authz hole given the unforgeable-identity design.
- **[high] No cancel_token / step_timeout on the aion path** (the legacy rhai path wires both) — cancelled workflows leak immortal agents holding token leases.
- **[high] Error strings violate aion's `retryable:`/`terminal:` prefix contract** → retry structurally impossible; pairs with Phase 2.2.
- Unbounded token-pool wait; activity `config` (retry/timeout/heartbeat) silently discarded on the norn route; non-idempotent `session_id` minting per attempt (replay pollution); god files (`assistant/service.rs` 1219, `norn_step.rs` 728, `catalog.rs` 2139, `agent.rs` 575); hardcoded `gpt-5.5` default; silent CWD-relative session fallback; unformatted/hand-spliced `dispatcher.rs`.

## Review-round decisions (2026-06-12, owner-approved)

The first Fable review of the Phase 0+1 change set returned NOT READY (8 blockers + issue list); a second fix round addressed everything. Decisions recorded from that round:

- **Approved overridable defaults** (per NO ASSUMED DEFAULTS, discussed and signed off): bash drain grace **2s**, provider retry backoff **1s**, rate-limit window **60s**, CLI request timeout **120s**, CLI max retries **2** — each now builder/config-overridable with these values as the explicit documented defaults.
- **Retry-After**: saturating arithmetic always (panic fixed); ceiling is an **optional** builder/config value — uncapped when unset, no invented default.
- **`runner.rs` (668 real LOC)**: explicitly deferred to **Phase 3 R2** — it pre-dates the change set, shrank during it, and R2's `StepState` rebuild would immediately redo a mechanical split.
- **Compaction summary**: owner chose to implement **LLM-written summarization now** (not Phase 2) — provider-backed summary of elided messages, mechanical digest retained only as a loudly-marked fallback on summarization failure.
- **Test-allow exception codified in CLAUDE.md** (was Phase 0 item 4): `#[allow]` permitted only on items inside `#[cfg(test)]`; never on production items.
- **`auto_compact_keep_recent_turns = 10`** (pre-existing default in `loop/config.rs`): blessed as the documented, overridable default — same treatment as the five approved values above.
- **Compaction summarization trade-offs (deliberate, revisit in Phase 3 R2):** the summarization call is single-attempt (failure = digest fallback, never a retry loop) and runs inside preflight outside the cancellation `select!` (a cancel during summarization waits one provider round-trip). Both flagged by the implementing track; accepted for now.
- **Index batching model:** registered sinks accumulate index deltas and flush at durability boundaries / `EventStore::checkpoint()` / drop; print + TUI checkpoint per turn; resume self-heals drifted entries. `DurabilityPolicy::Flush` genuinely performs no per-event fsync.

## Sequencing & verification

- Phases 0→1 are strictly ordered; within Phase 1, 1a–1c are independent of 1d–1f and can proceed in parallel tracks.
- Phase 2 items 1–2 (outcome + error taxonomy) should land **before** aion's retry work builds on string prefixes; the rest of Phase 2 can interleave with Phase 3.
- Every Phase 1 fix lands with a regression test reproducing the original failure (REVIEW.md documents the repro for each; four patch bugs already have reproducers from the review).
- Definition of done per CLAUDE.md: clippy `-D warnings` clean, fmt clean, full test suite green, no new `#[allow]`, no file crossing 500 non-test LOC, reviewed by a Fable-model subagent.
