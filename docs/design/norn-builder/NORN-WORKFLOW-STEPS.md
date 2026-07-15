# Using Norn Steps in Workflows

## What run_step_norn Is

`run_step_norn` is a Rhai builtin that runs an agent step in-process via Norn's AgentBuilder, instead of spawning a Claude CLI subprocess (which is what `run_step` does).

Same 4-parameter signature as `run_step`:
```rhai
let result = run_step_norn("step_name", "profile_name", "instruction text", "output_schema_json");
```

Returns the same shape: `{ is_error: bool, output: ..., error: "..." }`.

## Prerequisites (NOT YET MET)

Before `run_step_norn` works in production:

1. **Provider factory** must be wired in `dispatch.rs`. Currently passes `None` — meaning `run_step_norn` is never registered on the Rhai engine. Blocked on Tom's provider architecture decision (--bg mode, --bare flag, proxy routing).

2. **Norn-compatible profiles** must exist. Existing `.meridian/profiles/*.yaml` are authored for claude-runner. Norn profiles use markdown frontmatter or TOML with different fields (model, instructions, capabilities). A profile named in `run_step_norn("step", "developer", ...)` must resolve via Norn's `resolve_profile(name, scan_dirs)`.

3. **Local fixes committed**: `norn_translate.rs` has uncommitted improvements (bridge_summary, Lagged handling, text_delta naming). These need to land.

## How It Works (Once Prerequisites Are Met)

### Rhai Workflow Example

```rhai
// Scout step — read-only, no mutation tools
let scout_result = run_step_norn("scout", "scout", render_template("scout.md"), "");

// Dev step — full coding tools
let dev_result = run_step_norn("dev", "developer", render_template("dev.md"), output_schema);

// Check result
if dev_result.is_error {
    // Distinguish failure reasons:
    // "cancelled" — operator cancelled the workflow
    // "timed_out after Ns" — step_timeout elapsed
    // "schema_unreachable after N attempts: errors" — model couldn't produce valid schema
    // "max_iterations" — iteration cap hit
    print(`Dev failed: ${dev_result.error}`);
} else {
    // dev_result.output is the schema-validated JSON
    let code_changes = dev_result.output;
}

// Commit (same as with run_step)
commit("feat: implement the feature");
```

### Profile Requirements

A Norn profile at `.meridian/profiles/developer.md`:
```markdown
---
model: claude-sonnet-4-6
instructions: |
  You are a senior developer working in the yggdrasil codebase.
  Follow the coding standards in CLAUDE.md.
capabilities:
  - read
  - write
  - edit
  - bash
  - search
  - apply_patch
---
```

Or for a restricted review step at `.meridian/profiles/reviewer.md`:
```markdown
---
model: claude-sonnet-4-6
instructions: |
  Review the code changes. Do not modify files.
capabilities:
  - read
  - search
---
```

### Execution Budget

Profiles SHOULD declare execution budgets:
```markdown
---
max_iterations: 200
step_timeout: 900
---
```

Without these, a stuck step has no cap (except HTTP timeouts on individual API calls and the CancellationToken from workflow cancel).

### How It Differs from run_step

| Aspect | run_step | run_step_norn |
|--------|----------|---------------|
| Execution | Spawns Claude CLI subprocess | In-process via AgentBuilder |
| Events | ClaudeEvent JSONL from stdout | ProviderEvent on broadcast channel |
| Cancel | Process kill (SIGTERM) | CancellationToken (cooperative, within 1 iteration) |
| Profile format | claude-runner YAML | Norn markdown/TOML |
| Auth | SharedTokenPool (Claude OAuth) | Norn-owned Codex-compatible OAuth (`$NORN_HOME/auth/auth.json`) |
| Startup latency | ~300ms (process spawn) | ~0ms (function call) |
| State sharing | None (process boundary) | Shared Provider connection pool |

### Mixing run_step and run_step_norn

Both can coexist in the same Rhai workflow:
```rhai
// Use claude-runner for scout (cheaper, existing profiles work)
let scout = run_step("scout", "scout", instruction, "");

// Use norn for dev (in-process, streaming events, faster cancel)
let dev = run_step_norn("dev", "developer", instruction, schema);
```

### Observability

Every `run_step_norn` call produces:
- Real-time `text_delta`, `thinking_delta`, `tool_call`, `tool_result`, `completion` events in `execution_step_events`
- A `bridge_summary` event on step completion with `events_persisted` and `events_dropped` counts
- Standard `WorkflowStepStarted` / `WorkflowStepCompleted` service events on the event bus

### Cancel Behavior

When `meridian workflow cancel <execution-id>` fires:
1. `CancelHandle.cancel()` sets the AtomicBool AND notifies
2. Bridge task awaits the Notify, triggers CancellationToken
3. Norn's agent loop checks the token between iterations + select! on provider call
4. Current tool completes in full, then loop returns `AgentStopReason::Cancelled`
5. Adapter maps to `{ is_error: true, error: "cancelled" }` for the Rhai script

Propagation: within one iteration boundary (typically < 1 second).

## File Locations

- Adapter: `crates/meridian-services/src/workflow/imperative_callbacks.rs` (make_norn_step_runner)
- Event bridge: `crates/meridian-services/src/workflow/persistence/norn_translate.rs`
- Cancel handle: `crates/meridian-services/src/workflow/executor/mod.rs` (CancelHandle)
- Rhai registration: `crates/ygg-orchestrator/src/rhai_frontend/imperative_builtins.rs` (register_run_step_norn)
- AgentBuilder: `crates/norn/src/agent/builder.rs`
- Brief: `docs/design/norn-builder/briefs/NB-A1-workflow-adapter.md`
