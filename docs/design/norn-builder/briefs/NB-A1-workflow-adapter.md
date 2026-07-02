# Brief NB-A1 — Workflow adapter: make_norn_step_runner and event bridge

## Goal

Replace the claude-runner process-spawning StepRunner with an in-process Norn builder call. The adapter bridges the Rhai workflow engine's synchronous callback model to Norn's async agent loop, translates events for persistence, bridges cancellation, and adds run_step_norn() as a Rhai builtin alongside the existing run_step().

## Why

The current StepRunner spawns a Claude CLI process, parses its stdout, and kills it on cancel. This has three problems: Anthropic's ToS changes around headless Claude Code usage, process spawn overhead (~300ms per step), and the inability to share state between steps (each process is isolated). The Norn builder runs in-process, shares Provider connections, and returns structured output without stdout parsing.

## User Stories

- S1: As a workflow author, I want to call `run_step_norn("dev", "developer", "Fix the tests", schema)` in my Rhai script and get structured JSON back.
- S2: As a workflow operator, I want to see real-time tool call events in the UI while a Norn step runs, just like I see them with claude-runner steps.
- S3: As a workflow operator, I want to cancel a running workflow and have the Norn step stop within one iteration, not wait for a process kill timeout.
- S4: As a workflow author, I want to know WHY a step failed (timed_out, schema_unreachable, max_iterations, cancelled) so my Rhai script can handle each case differently.
## Requirements

### R1 — make_norn_step_runner function

**File:** `crates/meridian-services/src/workflow/imperative_callbacks.rs`

New function alongside existing make_step_runner. Takes Arc<dyn Provider>, working_dir, CancelHandle, event batcher sink, runtime handle, execution_id, step_counter. Returns StepRunner closure that calls AgentBuilder::new().profile().working_dir().output_schema().run_with() via runtime_handle.block_on().

**Acceptance:**
- Closure compiles against StepRunner type
- Provider is Arc::clone'd per call (shared connection pool)
- working_dir passed to builder per call
- Output schema parsed with error on malformed JSON (no silent fallback)
- Result maps AgentStopReason variants to distinct error strings: cancelled, timed_out, schema_unreachable, max_iterations
- NornError propagated as OrchestratorError::SessionError
- make_step_runner (old) remains for declarative/legacy workflows

### R2 — Event bridge: Norn events to persistence batcher

**File:** `crates/meridian-services/src/workflow/persistence/norn_translate.rs` (new)

Bridge task subscribes to AgentEvent broadcast channel, translates ProviderEvent variants to (event_kind, payload) tuples, constructs NewExecutionStepEvent, sends to existing BatchMessage::Event channel. Single persistence path — no dual storage.

**Acceptance:**
- ProviderEvent::TextDelta maps to event_kind "reasoning"
- ProviderEvent::ThinkingDelta maps to event_kind "thinking"
- ProviderEvent::ToolCallComplete maps to event_kind "tool_call"
- ProviderEvent::ToolResult maps to event_kind "tool_result"
- ProviderEvent::Done maps to event_kind "completion"
- ProviderEvent::Error maps to event_kind "error"
- step_name and step_number included in every persisted event
- No PersistentActions schema changes — event_kind + payload are already String + Value
- Bridge task shuts down when the broadcast sender is dropped

### R3 — Cancel bridge: Notify-based, zero-latency

**File:** `crates/meridian-services/src/workflow/imperative_callbacks.rs`

CancelHandle struct wrapping Arc<AtomicBool> + Arc<Notify>. service.cancel() sets the flag AND notifies. Bridge async task awaits notify then triggers CancellationToken. Zero polling, zero CPU.

**Acceptance:**
- CancelHandle { flag: Arc<AtomicBool>, notify: Arc<Notify> }
- cancel() sets flag to true and calls notify.notify_one()
- Bridge: notify.notified().await then token.cancel()
- Cancellation propagates within one iteration boundary
- Existing OrchestratorEngine cancel_flag check still works (reads same AtomicBool)
- ActiveExecutions stores CancelHandle instead of bare Arc<AtomicBool>

### R4 — run_step_norn Rhai builtin

**File:** `crates/ygg-orchestrator/src/rhai_frontend/cmd_builtins.rs`

Register run_step_norn(name, profile, instruction, output_schema) alongside existing run_step. Same 4-parameter signature, dispatches through make_norn_step_runner.

**Acceptance:**
- run_step_norn callable from Rhai with same parameters as run_step
- Returns same Value shape (is_error, output, error)
- Existing run_step unchanged
- Error variants are distinct strings the Rhai script can match on

## Checklist

- [ ] C1: make_norn_step_runner compiles against StepRunner
- [ ] C2: Schema parsing errors return OrchestratorError
- [ ] C3: AgentStopReason variants mapped to distinct error strings
- [ ] C4: Event bridge translates all ProviderEvent variants
- [ ] C5: step_name included in persisted events
- [ ] C6: No PersistentActions schema changes
- [ ] C7: CancelHandle struct with flag + notify
- [ ] C8: Zero-latency cancel propagation
- [ ] C9: ActiveExecutions uses CancelHandle
- [ ] C10: run_step_norn callable from Rhai
- [ ] C11: run_step (old) still works
- [ ] C12: Single persistence path
- [ ] C13: All existing workflow tests pass
- [ ] C14: Clippy clean

## Boundaries

- SHALL NOT modify any norn crate code
- SHALL NOT remove or modify make_step_runner
- SHALL NOT add PersistentActions schema changes
- SHALL NOT hardcode provider configuration
- SHALL NOT modify the StepRunner type alias
