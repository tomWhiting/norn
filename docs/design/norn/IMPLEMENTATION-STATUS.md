# Norn Implementation Status

Audit of `features-overview.md` (48 features) against the codebase as of **2026-07-02**.
Verified against the source tree on branch `hardening/final-state` (Waves 1–4 of the
final-state hardening campaign, HEAD `3c84682`). Every entry below was checked against the
code that exists now, not against git history or prior status docs.

Two items are deliberately **held, not wired** (see `docs/HOLD-FOR-DISCUSSION.md`) and are
listed in their own section; they must not be read as working features.

## Cross-cutting hardening (Waves 1–4)

These are not single numbered features but they shape the whole runtime and were the substance
of the campaign.

- **Single assembler (Wave 3, R1).** `AgentBuilder` (`agent/builder.rs`, phases in
  `agent/assembly.rs`) is the *only* runtime assembler. `Agent::into_parts()` → `AgentParts`
  (`agent/instance.rs`), `AgentBuilder::open_session` (`agent/builder_setters.rs`), and
  `SessionSpec::{Resume, ResumeLatestInWorkingDir, Fork, ForkLatestInWorkingDir, OpenOrResume}`
  (`agent/session_spec.rs`) are the front doors. The CLI, TUI, and JSON-RPC driven mode all
  assemble through `builder_from_cli` (`norn-cli/src/runtime/from_cli.rs`) → `AgentBuilder`.
  The CLI's former parallel assembly stack was deleted; the library assembler is `pub(crate)`,
  so a driver structurally cannot assemble an agent differently from a library embedder. A
  golden-snapshot conformance fence pins the overlay.
- **Explicit step runner (Wave 4, R2).** One agent step is an explicit state machine
  (`loop/runner/machine.rs`): `Gate → BuildRequest → CallProvider → Dispatch → ResolveStop`,
  each phase in its own module (`setup`, `prompt`, `provider_call`, `dispatch`, `stop`).
- **Deterministic tool ordering (Wave 4).** `ToolRegistry::names()` (`tool/registry.rs`)
  sorts, so the system prompt and the provider tool array are stable across runs — prompt
  caching holds.
- **Repo-wide compliance (Wave 4).** No production file exceeds 500 LOC; `mod.rs` files are
  declarations/re-exports only.
- **Provider core (Wave 1).** `StreamExecutor` (`provider/exec.rs`) is the shared streaming
  core. Structured error taxonomy: `NornError`, `ProviderError`, `ToolError`, `ConfigError`
  (`error.rs`).
- **Session resume (Wave 1).** Single-pass `ReplayArtifacts::from_events`
  (`session/persistence/replay.rs`) restores compaction marks and the action ledger in one
  traversal (`agent/assembly.rs::restore_session_state`).
- **Rules-engine lifecycle (Wave 2).** See feature-level note below — `SessionEvent::RuleInjection`
  is persisted, rule presence is rebuilt from history, and the `NestedScanner` is wired.
- **JSON-RPC driven mode (Wave 1).** `norn --protocol jsonrpc` (`norn-cli/src/print/driven.rs`,
  `print/jsonrpc/`), typed stop envelope, documented in `docs/design/norn-cli/DRIVEN-PROTOCOL.md`.
- **Macro coverage (Wave 1).** trybuild `compile_fail` / `ui` fixtures in `norn-macros/tests/`.

## Implemented

Features where the code matches what the doc describes.

| # | Feature | Key Files | Notes |
|---|---------|-----------|-------|
| 1 | Tool-Embedded Validation | `tools/write.rs`, `tools/edit.rs`, `tools/ast.rs`, `tools/validation.rs`, `tool/lifecycle.rs` | AST validation (tree-sitter `check_syntax`), file-length checks with glob overrides, read-before-overwrite gate. `RuntimePostValidateCheck` / `RuntimeOnSuccessAction` traits exist for external checks. Line counting is tokei-backed (`count_code_lines`, language-aware). **Caveat:** `ToolContext.runtime_args` (runtime-supplied tool arguments) is **held, not wired** — see Held section. |
| 2 | Headless, Scriptable Runtime | `lib.rs`, `agent/builder.rs`, `loop/runner/` | The `norn` crate is a library; `AgentBuilder` is the single assembler and one agent step is a function-driven state machine. Interactive front-ends (`norn-cli`, `norn-tui`, JSON-RPC driven mode) are thin drivers over the same builder. Example binaries (`examples/chat.rs`, `smoke.rs`, `login.rs`). |
| 3 | Schema-Enforced Structured Output | `loop/schema.rs`, `loop/runner/dispatch.rs`, `loop/runner/setup.rs` | `validate_against_schema()` via `jsonschema`. Dynamic `build_schema_tool()`. Validation feedback fed back to the model; retry budget configurable (`schema_attempt_budget`). |
| 7 | Input Channels / Steering | `loop/inbound.rs` | `InboundChannel` over `tokio::sync::mpsc`, `drain()` / `drain_if_steer_ready()` at tool boundaries. **Naming caveat:** the message enum is `MessageKind::{Steer, Update}` — there is no `FollowUp` variant here (`DeliveryMode` is the unrelated *rules* delivery enum). |
| 12 | Syntax-Aware Patch/Edit | `tools/patch.rs`, `tools/edit.rs`, `tools/ast.rs` | `ApplyPatchTool` with tree-sitter validation. `EditTool` with `check_syntax()` and `containing_symbols()` for syntactic blast-radius reporting. |
| 13 | LSP-Aware Tools | `tools/lsp/tool.rs`, `tools/lsp/backend.rs` | `LspTool` with injectable `LspBackend` trait: hover, definition, references, symbols, diagnostics. (LSP-driven *edit-time* blast radius is not wired — see Deviations.) |
| 15 | Direct Session Input | `loop/inbound.rs` | The steer path delivers directly into the session at the next boundary, bypassing the mailbox. |
| 16 | Sub-Agent and Forking | `agent/fork.rs`, `tools/agent/fork_tool.rs` | `ContextFilter` (`include_system`, `include_recent_n`, `exclude_tool_calls`), `ForkTool`, cooperative-cancellation cascade. Audit types are `ForkRequirement` / `OrphanToolCall` (there is no type named `ForkResult`). |
| 18 | Agent Registry | `agent/registry.rs` | `AgentRegistry` with **no** concurrency limits; `AgentEntry` (id, path, role, status, model, parent). Two-phase RAII spawn reservation (`SpawnGuard`). |
| 19 | Roles, Profiles, Capabilities | `profile/` | `Profile` (model, reasoning effort, tools, instructions, capabilities, settings, prompt commands), `Capability` composition, `resolve_profile` builds the gated registry. TOML and JSON. |
| 20 | Goals, Budgets, Continuation | `agent/goals.rs` | `GoalTracker` (token/time budgets), `ContinuationPolicy` (Stop/Handoff/Continue), `Scheduler` with `croner`. These are data structures consulted by the loop; there is no standalone background scheduler process. |
| 21 | Streaming Observability | `provider/traits.rs`, `provider/agent_event.rs` | Provider returns a `Stream` of `ProviderEvent` (text/thinking/tool-call deltas, done, error); broadcast for real-time streaming. |
| 22 | Session Trees | `session/tree.rs` | `SessionTree` (parent/child, metadata, status), `branch()`, `merge_summary()`, `BranchConfig`. Forking is an agent-level operation (`agent/fork.rs`); there is no `fork()` method on `SessionTree`. |
| 23 | Never Delete Audit Trail | `session/store.rs`, `session/context_edit.rs` | Append-only `EventStore`; `ContextEdits` uses suppress/supersede/inject marks. Events are never deleted. |
| 29* | Rules Engine (Wave 2) | `rules/engine.rs`, `rules/triggers.rs`, `rules/lifecycle.rs`, `rules/types.rs`, `context/scanner.rs` | Path-glob and bash/command triggers; delivery modes `SystemContextAppend` / `ContextInjection` / `MessageDelivery`; lifecycle tracking (`RulePresenceSet`) re-injects only after a rule leaves context; `SessionEvent::RuleInjection` is persisted and rebuilt on resume; `NestedScanner` wired into the loop. (*Not a numbered `features-overview` item; folded in here.) |
| 32 | Extension System | `integration/extensions.rs` | `ExtensionManifest`, `ExtensionProxyTool`, `ExtensionRegistry`; HTTP and Stdio transports. |
| 33 | OpenAI Responses API | `provider/openai/` | `OpenAiProvider` with SSE streaming, tool calling, OAuth via `codex` `auth.json`, ChatGPT backend auto-detection. A generic `provider/openai_compatible/` provider also exists. |
| 34 | Server-Side Web Search / Fetch | `tools/web/` | `WebSearchTool` (`server_side_tool_definition()` for the OpenAI web-search spec) and `WebFetchTool` (local HTTP fetch). |
| 35 | Subscription Optimization | `integration/claude/adapter.rs`, `provider/openai_oauth/` | Claude via `ClaudeRunnerAdapter` (implements `Provider`); OpenAI via OAuth. Two legitimate subscription paths. |
| 36 | Claude-Code-Compatible Tools | `tools/` | Tool names follow CC conventions where useful (Read, Write, Edit, Bash); input schemas are Norn's own. |
| 37 / 43 | MCP Server Exposure | `integration/mcp_server.rs` | `McpServer` exposes all registered tools via JSON-RPC on stdio (`initialize`, `tools/list`, `tools/call`). Launch with `norn mcp serve`. |
| 38 | Skills, Slash Commands, etc. | `tools/skill.rs`, `loop/commands.rs`, `profile/` | `SkillTool` (registered on the runtime-base path), `SlashCommandRegistry`, `preprocess_input()`, profile/capability system, session variables. |
| 39 | Session Variables | `integration/variables.rs` | `VariableStore` with Static / Shell (cached) / Computed sources; `expand()` substitutes `{{name}}` (double-brace) placeholders. |
| 40 | Runtime Prompt Commands | `profile/`, `loop/loop_context.rs` | `PromptCommand` (name, command, cache TTL); stdout populates dynamic system-prompt sections. |
| 41 | Hooks | `integration/hooks/` | `PreToolHook`, `PostToolHook`, `PreLlmHook`, `PostLlmHook`, `SessionEventHook`; `HookOutcome` (Proceed/Block/Modify); `HookRegistry`. |
| 42 | Standalone Modular Crates | crate structure | `norn` has no Meridian dependencies. |
| 47 | Diagnostic-Grade Agent Operations | `integration/diagnostics.rs`, `loop/` | `NornDiagnostic` / `DiagnosticCollector` are **now fully wired and populated during execution**: held on `LoopContext`, created on the runtime-base path, and `report()`ed live at three+ sites (tool errors, schema failures, permission denials) plus the rules engine. (Was passive/unwired in the previous audit — resolved.) |
| 48 | Full Spec Before Implementation | `docs/design/norn/` | DESIGN.md, VISION.md, features-overview.md, CHECKLIST.md, USER-STORIES.md; briefs landed. |

## Partially Implemented

Features where the core exists but parts are missing or scoped down.

### #4 — Per-Event Output Schemas

`loop/event_schemas.rs`: `EventSchemaSet` and `EventSchemaSet::validate()` exist and the
validation machinery is real. **Scope is minimal:** `EventType` currently has a single
variant (`Text`), not the eight-way event taxonomy the design describes.

### #5 — Dual Written and Spoken Responses

`SessionEvent::SpokenResponse` exists (`session/events.rs`) and is handled across
compaction / context / resume. **Missing:** there is no `spoken_response` *tool* — the model
has no registered surface to emit a spoken response.

### #6 — Tool Call Envelopes

`ToolEnvelope` (`tool/envelope.rs`) carries the open `metadata` field and `model_args`, and
these are live. **The third section — `runtime_inputs` (`RuntimeInputs`) — is held, not
wired** (always constructed `default()`, zero readers). See the Held section.

### #8 — Dynamic Tool Availability

No longer a stub. A real allow-list lives in `tool/registry.rs` (`set_available`, deny-wins),
with per-call runtime gating in `loop/tool_dispatch/gating.rs` (permission deny/ask).
Availability is applied at profile-resolve and child-spawn. **Missing:** model-driven,
mid-turn re-gating (e.g. remove Write when review begins, add frontend tools when editing
`.tsx`) is not implemented — gating is deterministic/config-driven, not toggled mid-session.

### #9 — Tool Search and Discovery

`ToolSearchTool` (`tools/tool_search.rs`) with BM25 scoring over `SharedToolCatalog`.
**Missing:** semantic / embedding-based search.

### #10 — Multi-Search Tool

`SearchTool` (`tools/search/`) supports content (regex), files (glob), fuzzy (nucleo), and AST
(tree-sitter). **Missing:** vector search, a distinct keyword mode, mode composition
(filters + ranking signals), and `fd` integration (file discovery uses `ignore::WalkBuilder`).

### #14 — Messaging and Collaboration

`agent/message_router.rs` (per-recipient monotonic sequence numbers) and
`agent/pending_messages.rs` (the former `agent/mailbox.rs`, now split). Coordination tools
`SpawnAgent`, `SignalAgent`, `WakeAgent`, `CloseAgent`, `Agents` exist. **Missing:** a
`trigger_turn` flag (does not exist), and integration with Meridian's collective
DMs/channels/identity (only profile-path references, no live integration).

### #17 — Model-Driven and Orchestrator-Driven Multi-Agent

Model-driven coordination tools are implemented (see #14). Orchestrator-driven Rhai builtins
are limited (see #31).

### #25 — Claude Code / Norn Session Translation

`integration/claude/wrapped.rs`: `NornWrappedClaudeCode` launches Claude Code bare-metal and
replays its stream-json events into Norn `SessionEvent`s (CC→Norn ingestion, tracks
`claude_session_id` for resume). **One-directional** — the reverse is only tool exposure via
MCP and system-prompt replacement, not full session reconstruction.

### #31 — Rhai Integration

`integration/rhai/`: `build_norn_engine()`, `register_norn_builtins()`, `NornRhaiContext`,
`AgentHandle` exist. Registered builtins are `spawn_agent` and `signal_agent` (overloaded).
**Missing:** a `RunScriptTool` (no rhai-executing tool is registered; `tools/script.rs` does
not exist), and the broader builtin surface (`run_agent`, `wake_agent`, `close_agent`,
`fork_agent`).

### #45 — Task Management Tools

`tools/task/`: `TaskTool`, `InMemoryTaskStore`, `TaskEntry`, `TaskStatus`. **Hierarchies are
implemented** (`create_subtask`, `children`, `ancestors`, `parent_task_id`) — no longer flat.
**Missing:** requirement linkage (no task↔design-system-requirement field/type).

## Not Yet Implemented

Explicitly deferred or not started. Most are Group 7 (session intelligence) in DESIGN.md.

| # | Feature | Status |
|---|---------|--------|
| 11 | Codex File Search as Baseline | No code, no brief. |
| 24 | Claude Code Transcript Editor | Deferred. No code. VISION.md marks it future. |
| 26 | DMs as High-Fidelity Memory | Deferred (Group 7). No DM-layer memory model. |
| 27 | Graph-Backed Session Intelligence | Deferred (Group 7). No Memgraph, no graph queries. |
| 28 | Hybrid Memory and Search | Deferred (Group 7). No embeddings/ColBERT/rerankers/vector search. |
| 29 | Workflow Reflection / Dream | Deferred (Group 7). No reflection mechanism. |
| 30 | NERVA Integration | Explicitly deferred. No code. |
| 44 | Design System Integration | Deferred (Group 7). No design-system crate integration. |
| 46 | libcorpus Integration | Deferred (Group 7). No libcorpus dependency. |

Also not yet built (subsets of partial features): semantic tool search (#9), vector/keyword/fd
search and mode composition (#10), the `spoken_response` tool (#5), the `RunScriptTool` and the
wider Rhai builtin surface (#31), and task↔requirement linkage (#45).

## Held for owner discussion — NOT working features

Both are scaffolding, deliberately not wired and not deleted, pending a design decision. See
`docs/HOLD-FOR-DISCUSSION.md`. Do not describe either as functioning.

- **`RunMonitored` (AI-monitored background tasks)** — `agent/monitor.rs`. `run_monitored`
  exists but is called only from its own test module; zero production callers; unused provider
  parameter; static-string heartbeat. (The live loop "iteration monitor" in `loop/iteration.rs`
  is a separate, unrelated mechanism.)
- **`ToolEnvelope.runtime_inputs` + `ToolContext.runtime_args`** — `tool/envelope.rs`,
  `tool/context.rs`. `RuntimeInputs` is always constructed `default()` with zero readers;
  `runtime_args` is only ever written `Value::Null` with zero readers.

## Deviations

### #18 — Agent Registry: no concurrency limits

`features-overview.md` #18 is titled "Bounded Concurrency," but the implementation deliberately
has **no** concurrency limit on agent count (per DESIGN). The "bounded" framing in the overview
is stale; the decision is no hardcoded limits.

### #13 — LSP blast radius is syntactic, not LSP-driven

The Edit tool reports containing symbols via tree-sitter (`containing_symbols()`), which is
syntactic. LSP-driven blast radius (find-references, affected callers) is available as
model-callable `LspTool` actions but is **not** wired into automatic edit post-validation.

### Naming drift from earlier docs

- `agent/mailbox.rs` was split into `agent/message_router.rs` + `agent/pending_messages.rs`;
  there is no `trigger_turn` flag.
- The inbound steering enum is `MessageKind::{Steer, Update}`; `DeliveryMode` is the *rules*
  delivery enum (`SystemContextAppend`/`ContextInjection`/`MessageDelivery`), not an inbound one.
- Fork audit types are `ForkRequirement` / `OrphanToolCall`; there is no `ForkResult`.
