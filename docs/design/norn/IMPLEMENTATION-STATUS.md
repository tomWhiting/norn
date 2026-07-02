# Norn Implementation Status

Audit of `features-overview.md` (48 features) against the codebase as of 2026-05-13.
Verified by Xenia Onatopp against `crates/norn/src/`.

## Implemented

Features where the code matches what the doc describes.

| # | Feature | Key Files | Notes |
|---|---------|-----------|-------|
| 1 | Tool-Embedded Validation | `tools/write.rs`, `tools/edit.rs`, `tools/ast.rs`, `tool/lifecycle.rs` | AST validation (tree-sitter), file-length checks with glob overrides (`LengthLimit`), read-before-overwrite gate, `CheckOverride` audit trail. `RuntimePostValidateCheck` / `RuntimeOnSuccessAction` traits exist for external linters. Runtime-supplied tool arguments via `ToolContext.runtime_args`. |
| 2 | Headless Scriptable Runtime | `lib.rs`, `loop/runner.rs` | Library crate, no binary. `run_agent_step` is a function call. Example binaries (`chat.rs`, `smoke.rs`, `login.rs`) prove end-to-end. |
| 3 | Schema-Enforced Structured Output | `loop/schema.rs`, `loop/runner.rs` | `validate_against_schema()` via `jsonschema` crate. Dynamic schema tool via `build_schema_tool()`. Validation feedback fed back to model. Retry budget configurable (`schema_attempt_budget`). |
| 4 | Per-Event Output Schemas | `loop/event_schemas.rs` | `EventSchemaSet` maps 8 `EventType` variants to JSON Schemas. Validation via `EventSchemaSet::validate()`. |
| 5 | Dual Written and Spoken Responses | `loop/event_schemas.rs`, `loop/tool_dispatch.rs`, `session/events.rs` | `SpokenResponse` event type. Dynamic `spoken_response` tool. Schema validation, session event recording, configurable tool name. |
| 6 | Tool Call Envelopes | `tool/envelope.rs` | `ToolEnvelope` with `model_args`, `runtime_inputs` (`RuntimeInputs`: inbound messages, diagnostics, filesystem changes), and open `metadata` field. |
| 7 | Input Channels / Steering | `loop/inbound.rs` | `InboundChannel` with `mpsc`. `DeliveryMode::Steer` (next boundary) and `DeliveryMode::FollowUp` (model-stop). `drain()` at tool boundaries. |
| 12 | Syntax-Aware Patch/Edit | `tools/patch.rs`, `tools/edit.rs`, `tools/ast.rs` | `ApplyPatchTool` with tree-sitter validation. `EditTool` with `check_syntax()` and `containing_symbols()` for blast-radius reporting. |
| 13 | LSP-Aware Tools | `tools/lsp/tool.rs`, `tools/lsp/backend.rs` | `LspTool` with injectable `LspBackend` trait. Actions: hover, definition, references, symbols, diagnostics. |
| 15 | Direct Session Input | `loop/inbound.rs` | `DeliveryMode::Steer` bypasses mailbox and goes directly into the agent session. |
| 16 | Sub-Agent and Forking | `agent/fork.rs`, `tools/agent/fork_tool.rs` | `ContextFilter` (include_system, include_recent_n, exclude_tool_calls). `ForkResult` with structured audit. `ForkTool` wraps infrastructure. |
| 17 | Dual Multi-Agent Modes | `tools/agent/`, `integration/rhai.rs` | Model-driven: `SpawnAgentTool`, `SendMessageTool`, `WaitAgentTool`, `CloseAgentTool`, `ForkTool`. Orchestrator-driven: Rhai builtins (`run_agent`, `spawn_agent`, etc.). |
| 18 | Agent Registry | `agent/registry.rs` | `AgentRegistry` with no concurrency limits. `AgentEntry` with id, path, role, status, model, spawned_at, parent_id. Two-phase RAII spawn reservation (`SpawnGuard`). |
| 19 | Roles, Profiles, Capabilities | `profile.rs` | `Profile` (model, reasoning_effort, tools, system_instructions, capabilities, settings, prompt_commands). `Capability` (name, tools, instructions, disallowed_patterns). `from_profile()` builds `LoopContext` + gated `ToolRegistry`. Supports TOML and JSON. |
| 20 | Goals, Budgets, Continuation | `agent/goals.rs` | `GoalTracker` with token/time budgets. `ContinuationPolicy` (Stop, Handoff, Continue). `Scheduler` with cron-based wake-ups (`croner`). |
| 21 | Streaming Observability | `provider/traits.rs`, `loop/runner.rs` | Provider returns `Pin<Box<dyn Stream<Item = Result<ProviderEvent>>>>`. `ProviderEvent` covers text/thinking/tool-call deltas, done, error. `broadcast::Sender<ProviderEvent>` for real-time streaming. |
| 22 | Session Trees | `session/tree.rs` | `SessionTree` with parent/child relationships, `SessionMetadata`, `SessionStatus`. `branch()`, `fork()`, `merge_summary()`. `BranchConfig` with `ContextFilter`. |
| 23 | Never Delete Audit Trail | `session/store.rs`, `session/context_edit.rs` | `EventStore` is append-only (`parking_lot::RwLock`). `ContextEdits` uses suppress/supersede/inject marks. Events are never deleted. |
| 31 | Rhai Integration | `integration/rhai.rs`, `tools/script.rs` | `build_norn_engine()`, `register_norn_builtins()`. `NornRhaiContext`, `AgentHandle`. `RunScriptTool` for inline Rhai execution. |
| 32 | Extension System | `integration/extensions.rs` | `ExtensionManifest`, `ExtensionProxyTool`, `ExtensionRegistry`. HTTP and Stdio transports. |
| 33 | OpenAI Responses API | `provider/openai/` | `OpenAiProvider` with SSE streaming, request building, tool calling, rate limiting. OAuth via `codex-login`. ChatGPT backend auto-detection. |
| 34 | Server-Side Web Search | `tools/web/search.rs`, `tools/web/fetch.rs` | `WebSearchTool` with `server_side_tool_definition()` for OpenAI web search spec. `WebFetchTool` for local HTTP fetch. |
| 35 | Subscription Optimization | `integration/claude/adapter.rs`, `provider/openai/` | Claude via `ClaudeRunnerAdapter`. OpenAI via `OpenAiProvider` with OAuth. Two legitimate subscription paths. |
| 36 | Claude-Code-Compatible Tools | `tools/` | Tool names follow CC conventions where useful (Read, Write, Edit, Bash). Input schemas are Norn's own. |
| 37 | MCP Server Exposure | `integration/mcp_server.rs` | `McpServer` exposes all registered tools via JSON-RPC on stdio. `initialize`, `tools/list`, `tools/call`. |
| 38 | Skills, Slash Commands, etc. | `tools/skill.rs`, `loop/commands.rs`, `profile.rs` | `SkillTool`, `SlashCommandRegistry`, `preprocess_input()`. Profile/Capability system. Session variables. `PromptCommand`. |
| 39 | Session Variables | `integration/variables.rs` | `VariableStore` with Static, Shell (cached), and Computed sources. `expand()` substitutes `{{name}}` placeholders. |
| 40 | Runtime Prompt Commands | `profile.rs`, `loop/loop_context.rs` | `PromptCommand` with name, command, cache_ttl. Evaluated each iteration. Stdout populates dynamic system sections. |
| 41 | Hooks | `integration/hooks.rs` | `PreToolHook`, `PostToolHook`, `PreLlmHook`, `PostLlmHook`, `SessionEventHook`. `HookOutcome` with Proceed/Block. `HookRegistry`. |
| 42 | Standalone Modular Crates | crate structure | `norn` has no Meridian dependencies. CO10 explicitly enforced. |
| 43 | MCP Server Exposure | `integration/mcp_server.rs` | Same as #37. |
| 48 | Full Spec Before Implementation | `docs/design/norn/` | DESIGN.md, VISION.md, features-overview.md, CHECKLIST.md, USER-STORIES.md. 24 norn briefs landed. 8 norn-cli briefs authored. |

## Partially Implemented

Features where the core exists but parts are missing or incomplete.

### #8 — Dynamic Tool Availability

`tool/availability.rs` is a 1-line stub (module doc comment only). No runtime tool gating by workflow stage, task state, or code location exists.

What IS implemented: `Profile::tools` provides a static allow-list, and `from_profile()` gates the `ToolRegistry`. But this is build-time gating, not dynamic runtime gating.

What's MISSING: the ability to add/remove tools mid-session based on context (e.g., remove Write when review begins, add frontend tools when editing `.tsx`). The trait or mechanism for this does not exist.

### #9 — Tool Search and Discovery

`ToolSearchTool` exists with BM25-style scoring and `SharedToolCatalog`. Functional for basic tool discovery.

What's MISSING: semantic search (embedding-based). Current implementation is keyword/BM25 only.

### #10 — Multi-Search Tool

`SearchTool` supports 4 modes: content (regex), files (glob), fuzzy (nucleo-matcher), and AST (tree-sitter queries).

What's MISSING from the doc's description:
- Vector search
- Keyword search (distinct from regex content search)
- Mode composition (using some modes as filters and others as ranking signals)
- `fd` integration (file finding uses glob, not fd)

### #14 — Messaging and Collaboration

`agent/mailbox.rs` has `MailboxSender`/`MailboxReceiver` with `trigger_turn` flag and sequence numbers. `SendMessageTool`, `WaitAgentTool` exist.

What's MISSING: integration with Meridian's collective messaging system. The mailbox is Norn's own lightweight implementation. When running inside Meridian, it should leverage the existing collective DMs/channels/member identity — this bridge does not exist.

### #45 — Task Management Tools

`TaskTool` exists with `InMemoryTaskStore`, `TaskEntry`, `TaskStatus`, `SharedTaskStore`.

What's MISSING: task hierarchies (parent/child), requirement linkage (connecting tasks to design-system requirements), workflow state tracking. Current implementation is flat.

### #47 — Diagnostic-Grade Agent Operations

`NornDiagnostic` and `DiagnosticCollector` exist with constructors for schema errors, tool blocks, and tool execution failures.

What's MISSING: the collector is not wired into `LoopContext` or the agent loop runner. It exists as a passive data structure but nothing populates it during execution. Agreed fix with Pythagoras: add `pub diagnostics: Option<Arc<DiagnosticCollector>>` to `LoopContext`, push from three sites in the runner (schema fail, pre-validate block, post-validate fail).

### Write Tool Line Counting

`WriteTool` uses `non_blank_line_count()` (naive: counts non-empty lines). The diagnostics crate has `count_code_lines()` using tokei as a library (language-aware, excludes comments and blanks).

Agreed with Pythagoras: swap for `diagnostics::languages::file_length::count_code_lines()`. Not yet done.

## Not Yet Implemented

Features explicitly deferred or not yet started. Most are Group 7 (session intelligence) in DESIGN.md.

| # | Feature | Status |
|---|---------|--------|
| 11 | Codex File Search as Baseline | No reference in codebase. Not briefs, not code. |
| 24 | Claude Code Transcript Editor | Deferred. No code, no brief. VISION.md mentions as future. |
| 25 | CC/Norn Session Translation | Deferred. `NornWrappedClaudeCode` captures CC→Norn events (one-directional), but no bidirectional translation. |
| 26 | DMs as High-Fidelity Memory | Deferred (Group 7). No DM-layer memory model. Mailbox carries messages but no layered drill-down linking to session events. |
| 27 | Graph-Backed Session Intelligence | Deferred (Group 7). No Memgraph integration. No graph queries. |
| 28 | Hybrid Memory and Search | Deferred (Group 7). No embeddings, ColBERT, rerankers, or vector search. |
| 29 | Workflow Reflection / Dream | Deferred (Group 7). No reflection mechanism. |
| 30 | NERVA Integration | Explicitly deferred. No code, no brief. |
| 44 | Design System Integration | Deferred (Group 7). No integration with design-system crate. |
| 46 | libcorpus Integration | Deferred (Group 7). No dependency on libcorpus. |

## Deviations

### #18 — Agent Registry: "Bounded Concurrency" vs No Limits

The features-overview.md says "bounded concurrency" but the implementation explicitly has NO concurrency limits. The code comment reads: "C66 forbids any field or constant that limits agent count." DESIGN.md confirms this is intentional. The features-overview.md text is outdated — the decision changed to no hardcoded limits.

### #13 — LSP Blast Radius

The doc mentions "LSP-based blast-radius analysis after function edits." The Edit tool reports containing symbols via tree-sitter (`containing_symbols()`), but this is syntactic, not LSP-driven. LSP-based blast radius (find references, affected callers) is not wired into tool post-validation. The `LspTool` provides these features as model-callable actions, but they are not part of the automatic edit lifecycle.
