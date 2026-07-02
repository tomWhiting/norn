# Brief NB-P2 — CancellationToken in agent loop

## Goal

Replace process-kill cancellation with cooperative in-process cancellation via tokio_util::sync::CancellationToken. The agent loop checks for cancellation between iterations and races it against provider streams, enabling clean abort with consistent EventStore state.

## Why

The current claude-runner cancellation model kills the child process (SIGTERM/SIGKILL). In-process Norn agents can't be killed — they share the process with the workflow engine. Cooperative cancellation via CancellationToken provides: instant response (no poll delay), clean state (EventStore records everything up to cancellation), and composability (parent and child agents share token hierarchies).

## User Stories

- S1: As a workflow operator, I want to cancel a running step and have the agent stop within one iteration boundary, so that cancellation is responsive.
- S2: As a workflow author, I want cancelled steps to return structured output (not an error), so my Rhai script can decide how to handle partial work.
- S3: As the builder API, I want cancellation to be optional (None = run until completion), so that non-workflow callers don't need to think about it.

## Requirements

### R1 — Add tokio-util dependency and CancellationToken parameter

**Files:** `crates/norn/Cargo.toml`, `crates/norn/src/loop/runner.rs`

Add tokio-util to norn's dependencies. Thread `cancel: Option<CancellationToken>` through `run_agent_step` and `run_agent_step_inner`. All existing callers pass None.

**Acceptance:**
- tokio-util in norn/Cargo.toml with sync feature
- run_agent_step signature includes cancel: Option<CancellationToken>
- run_agent_step_inner signature includes cancel: Option<CancellationToken>
- All existing callers (norn-cli, integration tests) compile with None
- No behavioral change when cancel is None

### R2 — Cancellation check between iterations

**File:** `crates/norn/src/loop/runner.rs`

At the top of the iteration loop in run_agent_step_inner, check `cancel.as_ref().map_or(false, |t| t.is_cancelled())`. If cancelled, return the new `AgentStepResult::Cancelled` variant with accumulated usage.

**Acceptance:**
- Cancellation checked before each provider call
- Cancelled returns AgentStepResult::Cancelled { usage }
- Usage reflects tokens consumed before cancellation
- EventStore is consistent (no partial writes)

### R3 — Race cancellation against provider stream

**File:** `crates/norn/src/loop/runner.rs`

Wrap the provider call (retry_with_backoff or call_provider) in `tokio::select!` racing against `cancel.cancelled()` when cancel is Some. If cancellation wins, return Cancelled with current usage.

**Acceptance:**
- Provider call is wrapped in select! when cancel is Some
- When cancel fires mid-stream, the provider future is dropped
- Usage from partial provider response is captured if available
- When cancel is None, no select! overhead (direct await)

### R4 — AgentStepResult::Cancelled variant

**File:** `crates/norn/src/loop/config.rs` (or wherever AgentStepResult is defined)

Add `Cancelled { usage: Usage }` variant to AgentStepResult.

**Acceptance:**
- New variant exists with usage field
- All match arms across the codebase handle the new variant
- Cancelled is distinct from MaxIterationsReached and TimedOut

## Checklist

- [ ] C1: tokio-util dep added with sync feature
- [ ] C2: CancellationToken parameter on run_agent_step
- [ ] C3: CancellationToken parameter on run_agent_step_inner
- [ ] C4: Cancellation check at iteration top
- [ ] C5: select! on provider call when cancel is Some
- [ ] C6: AgentStepResult::Cancelled variant exists
- [ ] C7: All existing callers pass None and compile
- [ ] C8: All match arms handle Cancelled
- [ ] C9: Test: cancelled before first iteration returns Cancelled
- [ ] C10: Test: cancelled mid-iteration returns Cancelled with partial usage
- [ ] C11: Test: None cancel runs to completion unchanged
- [ ] C12: Clippy clean

## Boundaries

- SHALL NOT change the Provider trait
- SHALL NOT add cancellation to individual tools — cancellation is at the loop level
- SHALL NOT make CancellationToken a required parameter — Option preserves backward compat
- SHALL NOT modify EventStore — it already handles partial sessions correctly
