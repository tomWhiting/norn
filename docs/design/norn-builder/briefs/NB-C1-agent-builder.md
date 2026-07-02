# Brief NB-C1 — AgentBuilder: fluent API for in-process agent execution

## Goal

A single builder type that composes all Norn runtime internals (ToolRegistry, EventStore, LoopContext, AgentLoopConfig, HookRegistry, DiagnosticInfra, Provider, profile resolution, system prompt) from simple inputs and exposes .run()/.run_with()/.run_stream() for execution. This is the public library API that workflows, tests, and embedding consumers call.

## Why

Norn's runtime is powerful but requires manual assembly of 8+ components to run an agent step. The builder hides that assembly behind a fluent API while exposing every capability — session persistence, streaming events, fork/spawn, cancellation, diagnostics, hooks, rules, structured output. Simple callers set 3-4 fields; advanced callers set 15+. Same type, same code path.

## User Stories

- S1: As a workflow step runner, I want to call `Agent::builder().provider(p).profile("dev").working_dir(path).run_with(instruction).await` and get structured output back.
- S2: As a workflow author, I want all tools available by default without having to list them, and the ability to exclude specific ones for read-only steps.
- S3: As a workflow author, I want to share a Provider across steps to reuse HTTP connections and rate limiting state.
- S4: As an agent operator, I want to configure retry policy per step — zero retries for budget-conscious scouts, aggressive retries for critical implementation steps.
- S5: As an orchestrator, I want to pass an EventStore from a previous run to resume a session with full context.
- S6: As a streaming consumer, I want .event_sender() to receive ProviderEvents as they happen, not just the final output.
- S7: As a parent agent, I want child agents spawned by fork/spawn to inherit my provider, hooks, and working_dir, with their own fresh EventStore.

## Requirements

### R1 — AgentBuilder struct and fluent API

**File:** `crates/norn/src/agent/builder.rs`

AgentBuilder::new(provider: Arc<dyn Provider>) returns a builder with all fields optional except provider. Fluent setters for every runtime capability. .build() validates and returns Agent. Agent exposes .run(), .run_with(prompt), .run_stream().

**Acceptance:**
- AgentBuilder::new(provider) compiles
- Every setter returns Self (fluent chain)
- .build() returns Result<Agent, NornError>
- .build() validates: provider is set (enforced by new()), tools or profile must provide at least one tool
- .run_with(prompt) is shorthand for .build()?.run_with(prompt) — consumes the builder
- Agent is not Clone (owns EventStore and runtime state)

### R2 — Profile resolution in norn lib

**File:** `crates/norn/src/profile/resolve.rs` (new, moved from norn-cli)

`pub fn resolve_by_name(name: &str, search_dirs: &[PathBuf]) -> Result<Profile, NornError>` scans directories in order, returns first match. Search order: .norn/profiles/ > .meridian/profiles/ > ~/.norn/profiles/. File precedence within a directory: .md > .toml > .json.

**Acceptance:**
- resolve_by_name finds profiles by name across search directories
- First-match-wins across directories
- .md > .toml > .json precedence within a directory
- Profile loads system_prompt, model, tools, capabilities from the matched file
- NornError::ProfileNotFound when no match
- Builder's .profile_name(name) calls this internally using working_dir-derived search paths

### R3 — Default all-tools and selective exclusion

**File:** `crates/norn/src/tools/registry_builder.rs` (new)

By default, the builder includes ALL registered Norn tools. The tool set is curated and purposeful — every tool exists for a reason. The builder exposes `.without_tools(&[names])` for callers that need to exclude specific tools (e.g., excluding mutation tools for a read-only scout step). The builder also accepts `.tool(custom_tool)` for adding custom tools beyond the standard set.

**Acceptance:**
- Builder with no .tools() or .without_tools() call includes all Norn tools
- .without_tools(&["bash", "write"]) excludes those specific tools
- .tool(custom_tool) adds a custom tool alongside the defaults
- Builder validates at .build() that at least one tool remains after exclusions
- Tool names used in without_tools match the actual Norn tool registry names

### R4 — AgentOutput and AgentStopReason types

**File:** `crates/norn/src/agent/output.rs` (new)

AgentOutput: output (Value), usage (Usage), event_store (Option<EventStore>), stop_reason (AgentStopReason). AgentStopReason: Completed, SchemaUnreachable { validation_errors, attempts }, MaxIterationsReached, TimedOut { elapsed, iterations }, Cancelled. Convenience methods: .text(), .structured_output(), .is_success(), .usage().

**Acceptance:**
- AgentOutput contains all fields
- AgentStopReason has all five variants
- .text() extracts the model's final text response from the EventStore
- .structured_output() returns the schema-enforced output Value when Completed
- .is_success() returns true only for Completed
- AgentStepResult maps cleanly to AgentStopReason (the builder does the conversion)

### R5 — Error model with configurable retry

**File:** `crates/norn/src/agent/builder.rs` (retry config), `crates/norn/src/provider/` (retry wrapper)

RetryPolicy { max_retries, initial_backoff, multiplier, max_backoff } configurable via .retry_policy(). Retried internally: RateLimited, ConnectionFailed, StreamError (5xx/timeout), StreamInterrupted. Never retried: AuthenticationFailed, ResponseParseError, UnsupportedFeature, HookBlocked. Default: 2 retries, 1s initial, 2x multiplier.

**Acceptance:**
- .retry_policy(RetryPolicy { max_retries: 0, .. }) disables retry
- .retry_policy(RetryPolicy { max_retries: 5, .. }) enables aggressive retry
- Default policy: 2 retries, 1s, 2x
- Rate limit errors retry with the policy's backoff
- Auth errors fail immediately regardless of policy
- Retry attempts are logged via tracing

### R6 — Builder .run() internals: assembly and execution

**File:** `crates/norn/src/agent/builder.rs`

.run(prompt) orchestrates: resolve working_dir → resolve profile → build ToolPreset into ToolRegistry → build ToolContext with working_dir → build AgentLoopConfig → build LoopContext → build system prompt → create/reuse EventStore → call run_agent_step with cancel → map AgentStepResult to AgentOutput.

**Acceptance:**
- .run_with("Fix tests") executes the full assembly and returns AgentOutput
- Profile overrides (model, system_prompt) are applied after profile load
- EventStore passed via .session() is reused (resume); otherwise fresh
- event_sender receives streaming events during execution
- CancellationToken is threaded to run_agent_step
- HookRegistry, DiagnosticInfra, Rules are wired when provided
- AgentRegistry is wired for fork/spawn when provided

## Checklist

- [ ] C1: AgentBuilder struct exists at norn/src/agent/builder.rs
- [ ] C2: AgentBuilder::new(provider) compiles
- [ ] C3: Every fluent setter returns Self
- [ ] C4: .build() validates and returns Agent
- [ ] C5: .run_with(prompt) executes and returns Result<AgentOutput, NornError>
- [ ] C6: Profile resolution works from norn lib (not norn-cli)
- [ ] C7: resolve_by_name scans directories in correct order
- [ ] C8: Builder with no tool config includes all Norn tools
- [ ] C9: .without_tools() excludes specified tools
- [ ] C10: .tool(custom) adds custom tool alongside defaults
- [ ] C11: AgentOutput has all fields and convenience methods
- [ ] C12: AgentStopReason has all five variants
- [ ] C13: RetryPolicy is configurable via builder
- [ ] C14: Default retry: 2 retries, 1s, 2x
- [ ] C15: Auth errors never retried
- [ ] C16: Rate limit errors retried with backoff
- [ ] C17: EventStore reuse works for session resume
- [ ] C18: event_sender receives streaming events
- [ ] C19: CancellationToken threaded to run_agent_step
- [ ] C20: Fork/spawn tools work via agent_registry
- [ ] C21: All existing tests pass
- [ ] C22: Clippy clean

## Boundaries

- SHALL NOT modify run_agent_step signature beyond the CancellationToken (NB-P2)
- SHALL NOT modify any existing Tool implementations (working_dir is NB-P1's scope)
- SHALL NOT add interactive/REPL features — those are norn-cli's domain
- SHALL NOT add workflow-specific logic — the builder is a general-purpose library API
- SHALL NOT hardcode model names, token limits, or other configurable values
