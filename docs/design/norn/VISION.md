# Norn: Agent Runtime for Yggdrasil

## Overview

Norn is a headless, embeddable agent runtime designed to be programmatically orchestrated by workflow systems. At its core it is a library crate: the `norn` crate exposes an `AgentBuilder` that assembles and runs agents, and an embedder — the orchestrator, Rhai scripts, the workflow engine, or any Rust program — drives it directly.

Norn also ships thin interactive front-ends over that same library. A CLI (`norn-cli`, the `norn` binary), a terminal UI (`norn-tui`), and a JSON-RPC driven mode (`norn --protocol jsonrpc`) all assemble their agent through the *exact same* `AgentBuilder` path an embedder uses (`builder_from_cli` → `AgentBuilder`). The CLI's former parallel assembly stack has been removed and the library assembler locked down (`pub(crate)`), so a front-end structurally cannot assemble an agent differently from a library consumer. Meridian is the flagship embedder. The library is the product; the front-ends are drivers.

The name comes from Norse mythology: the Norns are the three beings who tend to Yggdrasil, weaving the threads of fate at the base of the world tree. Urdr (past), Verdandi (present), Skuld (future).

### The Problem

Every agent harness in the landscape is a standalone terminal tool. They compete on startup time, memory footprint, and provider count. None of them are designed to be orchestrated. They are hammers. Norn is the factory.

The existing Meridian/Yggdrasil infrastructure already provides orchestration (Rhai scripting, workflow engine), profile management, diagnostic reporting, syntax analysis (tree-sitter, LSP), graph visualization (GraphMother), and Claude integration (Claude Runner). What is missing is the runtime underneath: an agent loop that is a function call, not a main function. Typed, schema-validated output as the contract between steps. AST-aware tool results. Graph-backed session intelligence. Direct context control.

### How Norn Fits

```
Meridian Collective (humans + agents, messaging, identity)
    |
Workflow Engine (Rhai scripting, step routing, profiles)
    |
    +-- Claude Runner (Claude Code sessions, Anthropic models)
    +-- Norn Runtime (direct LLM execution, OpenAI + future providers)
    |
Yggdrasil (source control, AST merging, stacked diffs)
    |
Infrastructure (diagnostics, LSP, syntax, corpus, design system, graph, embeddings)
```

Norn sits alongside Claude Runner as a second execution path. Claude Runner wraps Claude Code for Anthropic models with full subscription legitimacy. Norn provides direct LLM execution for OpenAI and future providers, with deeper control over the agent lifecycle than any wrapped CLI can offer.

Both paths share the same orchestration layer, profiles, tools, diagnostics, and infrastructure.

### Standalone Design

Norn follows the Meridian pattern: standalone crate first, integrated into Meridian second. Like Yggdrasil, the diagnostics crate, the LSP crate, Claude Runner, and libcorpus, Norn must be usable independently of Meridian. Any team, project, or system should be able to use Norn as a library without adopting the full Meridian stack.

This means Norn cannot depend on Meridian-specific types, messaging, or collective infrastructure at the crate level. Integration with Meridian happens through trait implementations, configuration, and the orchestration layer above.

---

## Provider Core

### Two Providers, Done Perfectly

Norn supports two providers with provider-specific optimizations. It is not a generic multi-provider compatibility layer.

**Claude** is accessed through Claude Runner and Claude Code. This is the legitimate subscription path for Anthropic models. Claude Runner handles session management, OAuth tokens, event parsing, and structured output via the `--json-schema` flag. Norn does not call the Anthropic API directly.

**OpenAI** is accessed directly via the Responses API. This supports streaming, tool calling, structured output via `response_format`, server-side web search, reasoning effort control, and the GPT-5.x model family. Norn implements this provider natively.

Future providers can be added, but Norn does not pursue provider count as a feature. Each provider is implemented with full awareness of its specific capabilities, pricing model, and subscription economics.

### Provider Trait

The provider trait abstracts the LLM call boundary:

- Accepts a context (system prompt, messages, tools, configuration).
- Returns a stream of typed events (text deltas, thinking deltas, tool call deltas, done, error).
- Reports usage (input tokens, output tokens, cache hits, cost).
- Supports provider-specific options (reasoning effort, response format, service tier).

### Subscription Economics

Provider selection is not just a technical decision. A Claude Max subscription provides substantial inference at a fixed monthly cost. An OpenAI Codex subscription currently provides roughly eight times the usage of Claude Max at a similar price point. Norn preserves these subscription advantages by using the legitimate subscription paths for each provider, never routing subscription credentials through unauthorized channels.

---

## Agent Loop

### Core Loop

The agent loop is a pure function: given a provider, tools, context, and instruction, execute the prompt-tool cycle and return typed output.

1. Construct context from system prompt, conversation history, and active tool definitions.
2. Call the provider with the context.
3. Stream the response. If the response contains tool calls, execute them.
4. Append assistant message and tool results to context.
5. If tool calls were made, loop back to step 2.
6. When the model stops (no tool calls, stop reason), validate output against the declared schema.
7. If validation fails, feed the error back to the model with the schema and the invalid output.
8. Return the validated, typed output.

### Schema-Enforced Structured Output

Every agent step can declare an output schema. The runtime validates the model's output against this schema before returning. If the output does not conform, the runtime injects the validation error and schema back into the context and prompts the model to try again.

For OpenAI, this uses `response_format` with JSON schema for native enforcement. For providers that do not support native schema enforcement, the runtime uses the validate-and-retry loop.

Structured output is not a feature flag. It is the contract between the agent step and the orchestrator.

### Per-Event Output Schemas

Schemas are not limited to the final response. Different event types in the agent loop can have their own schemas:

- **Assistant message**: structured text output with optional sections, references, and metadata.
- **Spoken response**: a parallel rendering of the same content optimized for text-to-speech. Dense paragraphs converted to natural spoken cadence. Especially important for accessibility (dyslexia, vision impairment).
- **Tool call envelope**: metadata wrapping the tool call including task linkage, purpose description, and runtime-supplied inputs.
- **Stop/final output**: the primary structured output matching the step's declared schema.
- **Question/clarification**: structured request for input with context about what is needed and why.
- **Handoff**: structured transfer of work to another agent or workflow stage.
- **Review/assessment**: structured evaluation with per-item verdicts and evidence.
- **Progress update**: structured status report with completion state and blockers.

Some schemas are enforced against model output. Some are envelopes populated by the runtime. Some are invisible to the model and used only by the orchestrator.

### Iteration Control

Hard iteration limits (stop after N tool calls) are optional and often the wrong primitive. The runtime supports multiple threshold types:

- **Token thresholds**: detect when context usage approaches the window limit.
- **Time thresholds**: detect long-running steps.
- **Repeated failure detection**: detect when the model is looping on the same error.
- **Context health signals**: detect degraded context quality (rising error rate, circular behavior).
- **Soft handoff**: at a configurable threshold (e.g., 80% of token budget), inject a message guiding the model to wrap up, summarize progress, and prepare for continuation.
- **Continuation paths**: when a threshold is reached, the runtime can compact context, fork to a fresh session with a structured summary, or hand off to the orchestrator for re-planning.

### Semantic Quality Signals

The runtime can detect semantic patterns in model output that indicate quality degradation:

- Hedging language ("for now", "this is sufficient", "we can revisit later") may indicate the model is avoiding hard work.
- Premature completion claims ("I've completed everything") when structured output shows incomplete items.
- Circular reasoning or repeated attempts at the same failed approach.

These signals are reported as runtime events, not hard stops. The orchestrator or a human reviewer decides how to act on them.

---

## Tool Framework

### Tool Trait

A tool has a name, description, input schema, a lifecycle with pre-validation, execution, post-validation, and on-success follow-up phases, and returns structured output.

- **name**: the tool's identifier (used in LLM tool definitions).
- **description**: what the tool does (included in the LLM's tool list).
- **input_schema**: JSON Schema for the model-supplied parameters.
- **pre_validate**: checks that run before execution. Can block the tool from running. Has both compile-time (baked into the tool implementation) and runtime (profile/policy-specified) phases. Example compile-time: the Write tool blocks if the target file exists and has not been read in this session. Example runtime: a profile rule requires reading TypeScript conventions before editing `.ts` files.
- **execute**: perform the action and return structured output.
- **post_validate**: checks that run after execution on the result. Has both compile-time (baked-in AST validation, file length checks) and runtime (configured diagnostic commands, linters, formatters) phases. If post-validation fails, the failure is reported as part of the tool output.
- **on_success**: follow-up actions that run when the tool succeeds. Compile-time: baked into the tool (e.g., edit tools always report the diff). Runtime: configured follow-ups (e.g., run `cargo clippy` after every edit, auto-format after write, commit after every successful edit in a specific workflow).

The compile-time phases are part of the tool implementation and cannot be overridden. The runtime phases are configured by profiles, policies, or the orchestrator and can vary per workspace, workflow, or task.

### Tool-Embedded Validation and Diagnostics

Tools do not merely perform actions and rely on external hooks. Validation and diagnostics are part of the tool lifecycle:

- **AST validation** (post-validate, compile-time): after editing a file, the tool parses the result with tree-sitter. If the edit introduced a syntax error, the tool reports the error before committing the change. This eliminates an entire class of failures that every other agent harness suffers from.
- **Read-before-overwrite** (pre-validate, compile-time): the Write tool blocks if the target file already exists and has not been read in the current session. The model must read first to understand what it is overwriting. Similarly, the Edit tool blocks if the file has not been read, since edits are unlikely to match without seeing the current content.
- **File length checks** (post-validate, compile-time + runtime): the runtime provides maximum file length policies. Write and edit tools enforce these. Policies can be path-specific (e.g., `mod.rs` files have a stricter limit than implementation files). The default limits are compile-time; path-specific overrides are runtime.
- **Diagnostics** (post-validate or on-success, runtime): after editing, the tool can run configured diagnostic commands (clippy, cargo check, tests, custom commands) and include the results in the tool output. These are profile/policy-specified, not baked in.
- **LSP integration** (post-validate, compile-time or runtime): after editing a function, the tool can report blast radius (affected symbols, references, diagnostics, related tests) using the LSP crate.
- **Pattern-specific policies** (runtime): different file patterns can have different validation rules, diagnostic commands, and quality thresholds.

### Runtime-Supplied Tool Arguments

> **Status (2026-07-02): held, not wired.** The `ToolContext.runtime_args` carrier
> exists but has no writer and no reader; it is deliberately not wired pending a
> design decision (see `docs/HOLD-FOR-DISCUSSION.md`). Policy that today *is*
> enforced — path/workspace confinement, file-length limits, the permission
> policy, per-tool config — reaches tools through the tool context and permission
> layer, not through this envelope field. The section below describes the intended
> design, not current behaviour.

Some tool inputs should not be set by the model. They are set by the runtime based on workflow policy, profile configuration, or workspace rules. Examples:

- Maximum file length for write/edit.
- Allowed/disallowed paths.
- Required post-edit diagnostic commands.
- Working directory constraints.
- Timeout limits for bash execution.

These arguments are part of the tool context, not the model's tool call. The model sees the tool's public schema. The runtime injects policy arguments before execution.

### Tool Call Envelopes

> **Status (2026-07-02): partially held.** The `ToolEnvelope` type carries the
> open `metadata` field (part 1) and the model-supplied arguments (part 2). The
> third part — `runtime_inputs` (`RuntimeInputs`: inbound messages, diagnostics,
> filesystem changes accumulated since the last tool boundary) — is scaffolded but
> has zero readers and is deliberately not wired: whether boundary signals ride the
> envelope or the now-existing message-injection / rules-engine paths is an open
> architectural decision (see `docs/HOLD-FOR-DISCUSSION.md`). In current behaviour
> those signals reach the model through the inbound channels and message router
> described under "Input Channels and Steering," not through the envelope.

Tool calls are wrapped in a runtime envelope with three parts:

1. **Metadata**: an open, schemaless field that the model can populate with whatever is useful. Tags, task references, plan linkage, descriptions, categories. The framework itself does not enforce or consume the metadata — it makes it available for downstream consumers (orchestrators, session graphs, audit systems, extensions). Profiles can optionally provide a metadata schema that the model should conform to, but this is configuration, not framework enforcement.
2. **Model-supplied arguments**: the actual tool parameters as defined by the input schema.
3. **Runtime-supplied inputs**: inbound messages, diagnostics, filesystem changes, working tree notifications, pending communications, or other signals accumulated since the last tool boundary.

The envelope gives the model rich context at each tool call without requiring explicit injection into the conversation. The runtime fills in the runtime-supplied section at each tool boundary.

### Effect-Based Parallel Scheduling

Tools declare their side effects. The runtime uses this to schedule concurrent execution:

- Read-only tools (read, grep, glob, find, search) can execute in parallel.
- Write tools (write, edit, patch) are serialized to prevent conflicts.
- Process tools (bash, shell) may be parallelized or serialized depending on configuration.
- Network tools (web search, web fetch) can execute in parallel.

### Bash Risk Classification

Shell commands are classified by risk level at runtime (not hardcoded at compile time):

- **Safe**: read-only commands (ls, cat, grep, find).
- **Low**: standard development commands (cargo build, npm install).
- **Medium**: write commands with bounded scope (git add, touch, mkdir).
- **High**: destructive commands (rm, git reset, force push).
- **Critical**: system-level commands (sudo, chmod 777, curl | bash).

Risk classification informs runtime permission policies, approval gates, and audit logging. The categories are evaluated dynamically and can be extended or overridden by configuration. The classifier is implemented independently, informed by patterns from existing implementations.

### Dynamic Tool Availability

Available tools change based on context:

- **Profile/role**: a team-lead profile gets workflow dispatch and coordination tools but not write/edit. A developer profile gets edit/write/LSP but not command-and-control. If an agent has edit access, its instinct is to fix things it finds, even when its job is to coordinate. Tool availability enforces role discipline.
- **Workflow stage**: during implementation, write/edit/bash are available. During review, they are removed and review/assessment tools are activated. During planning, planning and research tools are primary.
- **Code location**: when editing frontend files, frontend-specific diagnostics or component tools become available. When editing Rust files, cargo/clippy tools are prominent.
- **Task state**: once an implementation checklist is marked complete, implementation tools are gated and review tools are activated.

Dynamic tool availability is deterministic, configured by the profile/workflow/policy layer. It is not the model deciding which tools to use; it is the runtime deciding which tools to offer.

### Rules Engine

The rules engine is a standalone system that fires contextual guidance based on conditions. Rules are not tied to any specific tool. They are not hooks (hooks block, modify, or intercept; rules provide contextual information).

**Rule format**: rules are written with YAML front matter specifying trigger conditions, followed by the rule content (plain text guidance for the model).

**Trigger conditions**: two dimensions determine when a rule fires:

- **Path glob match**: fires when the agent reads or writes a file matching the pattern (e.g., `**/*.rs`, `**/mod.rs`, `crates/claude-runner/**`).
- **Bash/tool command match**: fires when the agent runs a command matching a pattern (e.g., `cargo test`, `git branch`, `rm -rf`). Can also match on any tool invocation, not just bash.

Each trigger can be configured to fire **before** or **after** the matched action.

**Delivery modes**: three ways a rule can reach the model:

- **System context append**: the rule is added to the system prompt for the remainder of the session. Used for workspace-wide conventions that should persist (e.g., Rust coding standards activated on first `.rs` read).
- **Context injection**: the rule is picked up through the agent loop's input channels and delivered at the next tool boundary. Used for situational guidance (e.g., "if you are about to run cargo test, consider using yg diagnostics instead").
- **Message delivery**: the rule appears as a message in the conversation. Used for reminders or one-time notes (e.g., "great work completing that task, remember to also update the plan and documentation").

**Lifecycle and anti-spam**: rules track whether they are currently present in the active context. The engine knows when a rule has been edited out of context (by compaction, context management, or natural context window aging). The logic:

- If the rule is currently in context, do not re-inject on trigger.
- If the rule has been removed from context (the engine detects this), re-inject on the next trigger.
- No fixed cooldown timers. The engine adapts to actual context state.

This is the most complex lifecycle approach but the only correct one: it does not spam, it does not miss, and it adapts to context editing.

**Examples**:

- First time the agent reads a `.rs` file: append Rust conventions to system context. These stay until edited out. If compaction removes them and the agent reads another `.rs` file later, they are re-injected.
- Agent is about to run `cargo test`: inject a pre-action rule suggesting `yg diagnostics` instead, which provides better structured output.
- Agent is about to run `git branch`: inject a reminder to check whether other agents are working in the repository.
- First time the agent reads a file in a new directory: the rule executes a shell command to produce a file tree listing, which is injected as context so the agent knows the directory structure.
- Agent completes a task (updates task status): inject a post-action reminder about updating plans and documentation.

**Shell execution in rules**: rules can include commands that execute at injection time to produce dynamic content. Team status, file trees, diagnostic summaries, and other runtime information can be generated on the fly and injected as rule content.

**Relation to hooks**: rules and hooks are complementary. Rules say "you should know this." Hooks say "stop, I am checking something" or "I am modifying this." A rule cannot block a tool call. A hook can. Blocking rules may be considered later but are not the initial model.

### Tool Search and Discovery

For large tool registries (especially when MCP servers expose many tools), BM25 or semantic search over the tool catalog lets the model discover tools dynamically. The model calls a `tool_search` tool with a query and receives matching tool definitions it can then invoke.

Deterministic tool gating by profile/stage is preferred where possible. Tool search is the fallback for large, dynamic tool sets.

### Claude Code Tool Names

Some tool names naturally match Claude Code's conventions (Read, Write, Edit, Bash, Grep, Glob, WebSearch, WebFetch) because the underlying operations are the same. Norn does not contort its tools to match Claude Code schemas when Norn's tools are substantively better.

Where Norn tools are enhanced (AST-aware edit, diagnostics-aware write, multi-search), they should have their own names and schemas. These enhanced tools can be exposed to Claude Code as MCP tools, allowing Claude to use Norn's stronger tools instead of its native ones.

### Core Tools

The initial tool set:

**File operations:**
- **Read**: read files with line numbers, image support, binary detection.
- **Write**: create or overwrite files, with AST validation, file length policy, read-before-overwrite enforcement, and configurable post-write diagnostics.
- **Edit**: string replacement editing, with AST validation, diff output, blast-radius analysis, read-before-edit enforcement, and configurable post-edit diagnostics.
- **ApplyPatch**: unified diff application with tree-sitter syntax awareness.

**Search (unified):**
- **Search**: combined ripgrep (content search), fuzzy file search (nucleo), fd-style file discovery, glob filtering, AST/structural search (ast-grep), vector search, and keyword search. Modes can be used independently or composed as filters and ranking signals. Subsumes the functionality of dedicated grep, glob, and find tools.

**Shell:**
- **Bash**: shell command execution with runtime risk classification, timeout, streaming output, progress detection.

**Web:**
- **WebSearch**: OpenAI server-side web search for Codex/GPT models; local implementation (DuckDuckGo HTML or similar) for non-OpenAI models.
- **WebFetch**: HTTP fetch with HTML-to-markdown conversion.

**Code intelligence:**
- **LSP**: language server protocol integration as a tool. Hover, go-to-definition, find references, document symbols, workspace symbols, diagnostics, code coverage, and extensible diagnostic sources (cargo clippy, custom linters). Configurable per workspace.

**Agent coordination (model-driven):** as implemented, the tools are —
- **SpawnAgent**: spawn a sub-agent with a task, model, role, and optional forked context.
- **SignalAgent**: send a signal to another agent (message / steer / fyi) — the messaging primitive.
- **WakeAgent**: wake or wait on one or more agents (completion or inbound signal).
- **CloseAgent**: close an agent and cascade to its descendants.
- **Fork**: fork the current agent onto a different model for a bounded task. Returns a structured audit result.
- **Agents**: query the agent registry (statuses, paths, roles) from within a session.

**Workflow and task:**
- **Skill**: activate a skill (load a SKILL.md prompt template into context).
- **Task**: task management (create, list, update, complete). Compatible with Claude Code task format where useful, extended toward richer task hierarchies and requirement linkage.
- **RunScript** (working name — needs a better name that conveys "scripted workflow" rather than "run a script"): write and execute an inline Rhai script. The script can call host functions (ripgrep, file operations, spawn agents via Claude Runner or Norn, aggregate results). Returns structured output. Useful for ad-hoc automation: "find all files with unwrap, send each to an agent to fix" or "list changed files in last diff, generate documentation for each." This tool and the broader scripting/package system around it deserve further design discussion.

**Discovery:**
- **ToolSearch**: BM25 or semantic search over the tool catalog for large/dynamic tool sets (especially MCP servers).

---

## Input Channels and Steering

### Inbound Channels

The runtime supports inbound channels while the agent loop is running:

- **User messages**: steering from a human operator.
- **Agent messages**: messages from other agents in the collective.
- **Diagnostics**: new diagnostic results from background processes.
- **Filesystem changes**: working tree modifications detected by file watchers.
- **Workflow signals**: notifications from the orchestrator (stage transitions, cancellations, priority changes).

These inputs do not interrupt the agent. They are accumulated and delivered at controlled points in the loop: after the current tool batch completes, before the next LLM call. The model receives them as part of the tool envelope's runtime-supplied section.

### Steering Without Interrupting

When a user or orchestrator sends a steering message ("stop that, check the MCP server first"), the message is queued and delivered at the next tool boundary. The model sees it in context and can adjust its behavior without a hard interrupt.

Two delivery modes:

- **Steer**: delivered after the current tool batch, before the next LLM call.
- **Follow-up**: delivered only when the agent would otherwise stop (no more tool calls).

### Direct Session Input

Some inputs bypass the messaging layer and go directly into the agent session:

- Slash commands (compaction, model switch, abort).
- Compaction commands.
- Urgent steering that must be processed immediately.

---

## Context Control

### Surgical Context Editing

Context editing in Norn is not compaction (summarize old stuff, keep recent). It is precise, message-level control:

- **Keep**: retain this message in full.
- **Suppress**: exclude this message from prompt construction (but preserve in the audit trail). Named "suppress" rather than "discard" to make it unambiguous that the data is retained, not deleted.
- **Summarize**: replace a sequence of messages with a structured summary.
- **Inject**: add context from an external source (another agent's output, a research finding, a plan update).
- **Compact**: summarize everything before a cut point, keeping recent messages in full.

### Immutable Audit Trail

For Norn-native sessions, context editing never destroys history. Events are marked as superseded, compacted, summarized, skipped for prompt construction, or replaced by a derived memory. The original events remain in the session store.

Prompt construction is a view over the event stream, not a mutation of it. The system chooses what to include, summarize, or skip when building the next prompt. The underlying data is immutable.

### Claude Code Transcript Editing

Claude Code session files are external, dependency-sensitive artifacts owned by Claude Code's format. Editing them requires surgical precision. The transcript editor must preserve chains, dependency graphs, and valid transcript structure.

Lesson learned: destructive context editing without preserving the dependency graph will shred the session. The transcript editor must understand the structural relationships between messages (assistant messages reference tool calls, tool results reference tool call IDs) and maintain them.

### Claude Code / Norn Session Translation

Eventually, Norn should be able to ingest Claude Code session transcripts into its own tree/session model and reconstruct valid Claude Code transcripts when invoking Claude Code through Claude Runner. This keeps execution inside Anthropic's terms while giving Norn its own session intelligence.

---

## Multi-Agent

### Dual-Mode Coordination

Norn supports two modes of multi-agent coordination that work together:

**Orchestrator-driven**: the workflow engine (Rhai scripts) spawns agents with specific tasks, routes structured outputs between them, and decides what happens next. This is deterministic and reproducible.

**Model-driven**: an agent has tools to spawn sub-agents, send messages, wait for results, and close agents. The model decides when to delegate. This handles unexpected situations where the agent needs help it wasn't planned for.

Both modes share the same agent registry, messaging infrastructure, and concurrency controls.

### Forking as a First-Class Primitive

An agent can fork itself. The fork inherits the parent's context (optionally filtered: keeping meaningful conversation, stripping tool call noise). The fork can run on a different model (cheaper, faster) for bounded tasks:

- **Commit**: fork onto a lighter model to stage files, write a commit message, and commit. The parent receives a structured audit result.
- **Message triage**: fork to respond to an incoming message without derailing the main task.
- **Log analysis**: fork to analyze noisy command output ("find the relevant errors in this log").
- **Research**: fork to explore a tangent without polluting the main context.

The fork's audit trail (what it did, what it produced, how many tokens it used) is reintegrated into the parent's session graph.

### Command Wrappers

A lightweight pattern built on forking: run a noisy command and have a forked agent analyze the output. "Find the relevant errors." "Which failures need attention first?" "Summarize this test output." The fork processes the output and returns a structured summary to the parent.

### Agent Registry

The runtime maintains an agent registry tracking:

- Active agents and their statuses (running, idle, completed, errored).
- Hierarchical agent paths (e.g., `/root/researcher/subtask`).
- Roles and model configuration per agent.
- Two-phase spawn reservation: a spawn slot is reserved atomically before the agent is created. If creation fails, the slot is released automatically (RAII pattern).

There are no hardcoded concurrency limits on agent count. In practice, the system may run dozens of concurrent agents. Any "reasonable default" limit will be wrong and cause problems. Resource constraints (provider rate limits, system memory, API quotas) are handled at the provider and infrastructure level, not by capping agent count in the registry.

### Messaging

Agents communicate through mailboxes. When integrated with Meridian, Norn should leverage the existing collective messaging system (currently being ported from Meridian v1), which already provides DMs, channels, and member identity. When running standalone, Norn provides its own lightweight mailbox implementation:

- Each agent has a mailbox (sender + receiver).
- Messages carry author path, recipient path, content, and a `trigger_turn` flag.
- `trigger_turn: false` (message): queue without waking. The recipient sees it at the next natural boundary.
- `trigger_turn: true` (followup task): start a new turn immediately.
- Sequence numbers on the mailbox enable efficient wait-for-any without polling.

### Roles and Profiles

Meridian already has a profile and capability system that configures model, reasoning effort, tools, hooks, settings, instructions, disallowed patterns, and composable capabilities. Norn should integrate with the existing profile system rather than building a parallel role concept.

When running standalone (outside Meridian), Norn provides a simplified role configuration:

- Model and reasoning effort overrides.
- Tool set overrides (which tools are available).
- Instruction overrides (system prompt additions).
- Configuration overlays (any setting can be role-specific).

The existing Meridian profiles (e.g., architect, developer, reviewer, scout) and capability composition (team lead + workflow dispatch, team lead + planning/research/review) should map naturally onto Norn's role system.

### Goals and Budgets

Agents can have active goals with:

- **Objective**: what the agent is trying to achieve.
- **Token budget**: optional cap on total token usage. When reached, the agent receives a steering message to wrap up.
- **Time budget**: optional wall-clock limit.
- **Continuation policy**: what happens when a threshold is reached (compact and continue, fork with summary, hand off to orchestrator, stop and report progress).
- **Status tracking**: active, paused, budget-limited, complete.

When a goal is active and the agent becomes idle, the runtime can automatically start a continuation turn.

### Batch Processing

For data-processing workloads, the runtime supports fan-out patterns:

- Given a CSV or structured input, spawn parallel agents.
- Each agent processes one item and reports a structured result.
- Results are aggregated into a structured output (CSV, JSON, or custom format).

### AI-Monitored Background Tasks

> **Status (2026-07-02): held, not wired.** `RunMonitored` (`agent/monitor.rs`) is
> scaffolding only — zero production callers, an unused provider parameter, a
> static-string heartbeat. It is deliberately not wired and not deleted, pending a
> design decision on whether a monitored task should be a bespoke monitor type or is
> now better expressed as a persistent child agent plus watch rules (the wake/linger,
> `signal_agent`, and delegation-budget machinery landed after this scaffolding was
> written). See `docs/HOLD-FOR-DISCUSSION.md`. The section below is the intended
> design, not a working feature.

Long-running commands and sub-agents can be monitored by a lightweight model instead of consuming the parent agent's context:

1. Agent spawns a long-running command (build, test suite, server process) or a sub-agent.
2. A cheap, fast model watches the output stream.
3. The monitor can answer questions about progress, detect errors and alert the parent, provide structured summaries on demand, and watch for specific patterns.
4. The parent agent continues working and queries the monitor when it needs an update.

This avoids the fundamental problem with current sub-agent models: the parent either blocks waiting for the sub-agent, or it reads the full output and consumes all the context it was trying to save by delegating.

The monitor pattern applies to:
- Background shell commands (build, test, deploy, server processes).
- Sub-agents performing research, documentation, or analysis.
- Any long-running operation where the parent needs structured updates without raw output.

The tool shape is `RunMonitored`: takes a command or agent task, a monitoring model (default to cheap/fast), and monitoring instructions (what to watch for). Returns a handle for queries, alerts, and completion summaries.

### Scheduling and Wake-ups

Agents can schedule future work without keeping a session alive:

- **Session-active cron**: the agent session stays alive and wakes up on schedule. Simple but resource-wasteful.
- **Session-dispatched cron**: the agent session ends, but a scheduled job re-launches the session at the scheduled time with saved state and instructions. No idle sessions consuming resources.

The dispatched mode is preferred. The cron service is a lightweight process that fires wake-ups. When a wake-up fires, it launches a fresh Norn session with the saved context. When integrated with Meridian, this hands off to Meridian's existing scheduling infrastructure.

---

## Session Intelligence

### Session Event Model

Sessions are an immutable append-only event stream. Event types include:

- Message events (user, assistant, tool result).
- Model/thinking level changes.
- Compaction events (summary, cut point).
- Fork events (parent-child relationship).
- Label events (named checkpoints).
- Custom events (application-defined data).

Events have IDs and parent IDs forming a tree. Branching, forking, and reintegration are structural operations on the tree.

### Graph-Backed Sessions

Session events, DM exchanges, forks, sub-agent traces, code entities, requirements, tasks, and outputs are connected in a graph. Initial implementation uses Memgraph for graph storage and query.

Graph queries enable:

- "Which agent introduced this bug?" (causal chain traversal).
- "What context led to this decision?" (path from decision to inputs).
- "Which decisions from session A informed session B?" (cross-session edges).
- "Show me everything that changed as a result of this plan step." (impact analysis).

### DMs as High-Fidelity Memory

The message-level exchange between humans and agents is often the highest-signal memory available. A conversation where an agent works for 25 minutes and then sends a dense summary captures intent, result, reasoning, and direction without drowning in intermediate tool calls.

DM events are linked to the lower-level session event logs. Memory has layers:

- **DM layer**: high-level summaries, decisions, directions.
- **Structured workflow output layer**: typed outputs from each workflow step (scout findings, plan tasks, developer notes, review verdicts).
- **Session event layer**: detailed tool calls, edits, diagnostics.
- **Fork/sub-agent layer**: delegated work traces.

Drilling down follows the graph: start from the DM summary, trace to the workflow output, trace to the session events, trace to the fork.

### Hybrid Memory and Search

Memory retrieval combines multiple signals:

- Dense embeddings (via Tezera or configured embedding service).
- Sparse embeddings (BM25, keyword).
- ColBERT-style late interaction.
- Rerankers.
- Keyword and regex search.
- Graph queries (traversal, path finding, neighborhood).

### Workflow Reflection

Agents should periodically reflect on past runs, outcomes, messages, and lessons to extract improvements:

- What patterns led to success?
- What patterns led to failure?
- Which briefs took longer than expected and why?
- What context was missing that would have helped?

Reflection can be a scheduled process (a "dream program") that wakes an agent to review its own history and update memory.

---

## Integration

### Rhai Integration

Rhai is the scripting layer for workflow definitions. Norn exposes Rhai builtins for agent operations through `build_norn_engine()` / `register_norn_builtins()`. As implemented, the registered builtins are:

- `spawn_agent(...)` — spawn a concurrent agent, returning an `AgentHandle`.
- `signal_agent(...)` — signal an agent (message / steer / fyi), overloaded across signal kinds.

The `RunScriptTool` runs inline Rhai against this engine. The broader builtin surface (a synchronous `run_agent` step call, `wake_agent`, `close_agent`, `fork_agent`) is intended but not yet all registered; the model-driven tool surface (SpawnAgent / SignalAgent / WakeAgent / CloseAgent / Fork / Agents) is the fuller coordination path today.

Beyond builtins, Rhai may serve as an extension language with sandboxed host functions for HTTP, file search, tool registration, event handlers, and workflow hooks.

### Extension System

Norn uses the Meridian extension system for all out-of-process extensions. This is not a new extension platform — it is the same system that already powers text-to-speech, speech-to-text, voice agents, and other Meridian extensions. Norn becomes another consumer surface alongside the web view and future TUI.

The Meridian extension system uses a manifest-based registration model. Extensions are external processes that register with a manifest declaring their capabilities, serve their own HTTP routes, and optionally provide frontend components. The extension protocol handles handshake, settings, lifecycle, and component registration. SDKs exist in Rust (`meridian-ext` crate), Python (`meridian-ext-py`), and TypeScript/Node (`meridian-bridge`), with Deno as a future option.

Reference: the existing extension system lives at `meridian/crates/meridian-ext` (Rust SDK, not yet ported to v2) with example extensions at `meridian-extensions/` (speech, productivity, agents).

**How Norn integrates:**

Extensions declare which surfaces they apply to. An extension might apply to the web view only (a collaborative planning whiteboard), to Norn only (a custom tool or diagnostic provider), to both (text-to-speech that works in the web view AND in agent sessions), or to the TUI when it exists.

For Norn specifically, extensions can:
- Register custom tools (the extension serves the tool execution endpoint, Norn calls it via the protocol).
- Subscribe to agent lifecycle events (tool calls, messages, completions).
- Provide diagnostic sources (linters, code coverage, custom checks).
- Provide embedding/search services (one process, shared across all agents).
- Provide frontend components for the Meridian web view (React components loaded in the browser).

**Shared runtimes:** Extension processes are shared across Norn instances. If 14 agents are running and they all use the same embedding extension, there is one Python process with the model loaded and 14 agents connected to it. No duplication. The runtime spawns on first use and stays alive as long as any agent is connected.

**Language independence:** Because extensions are out-of-process and communicate via protocol, they can be written in any language. The Rust, Python, and TypeScript SDKs are convenience layers over the protocol. Deno support is a natural future addition (pure Rust V8 via rusty_v8, full npm/TypeScript ecosystem).

**In-process scripting remains Rhai:** The RunScript tool, workflow definitions, and orchestration logic continue to use Rhai. Rhai is fast, lightweight, and sandboxed for in-process execution. It does not need to be an extension language — the extension system handles that.

**Compiled Rust extensions:** For performance-critical in-process components (custom tool implementations, AST analyzers, diagnostic providers), Rust traits linked at build time remain an option. These are not extensions in the runtime sense — they are pluggable components compiled into the binary.

### Claude Runner Integration

Norn and Claude Runner coexist as execution paths:

- Profiles specify `runner: claude` or `runner: norn` (or both, for hybrid workflows).
- The orchestrator routes steps to the appropriate runner based on profile configuration.
- Both runners produce `StepOutcome` values that the orchestrator handles uniformly.

### Norn-Wrapped Claude Code

A third integration mode: Norn wraps Claude Code CLI, stripping it to bare metal and providing Norn tools via MCP.

- Claude Code launches with all native tools disabled, system prompt replaced, and settings deactivated.
- Norn provides its enhanced tools (AST-aware edit, diagnostics-aware write, search, task management) as an MCP server that Claude Code connects to.
- Norn captures all Claude Code output events and converts them into Norn's session format for graph-backed storage and analysis.
- Claude Code retains its own session history for legitimate session resumption via `--resume`.

This gives Claude models access to Norn's tool ecosystem while staying fully within Anthropic's terms of service. Claude Code is still the harness; Norn provides the tools and captures the intelligence. The tradeoff is Claude Code's 1-2 second startup time, which is acceptable.

Note: OAuth token pool rotation is a Meridian-specific optimization for users with multiple Claude Max subscriptions. It is not part of Norn's core and is not required for single-subscription usage.

### MCP

**MCP client**: Norn connects to external MCP tool servers. MCP tools are registered dynamically and appear in the tool registry alongside native tools.

**MCP server**: Norn can run as an MCP server itself, exposing all its tools to other agents and harnesses. This works the same way Claude Code exposes its tools as an MCP server. Other agents (including Claude Code sessions) can connect to Norn's MCP server and use its enhanced tools (AST-aware editing, multi-search, diagnostics-aware write, LSP, task management) without Norn being the primary harness. Start it with `norn mcp serve` (JSON-RPC over stdio).

This is distinct from the Norn-wrapped Claude Code integration mode. In MCP server mode, Norn is not wrapping anything — it is simply making its tools available over the standard MCP protocol for any consumer.

### Diagnostics Integration

Agent operations feed into the diagnostics crate with the same quality level as compiler diagnostics:

- Tool call failures with structured error context.
- Schema validation failures with the expected schema and actual output.
- Policy violations (file length, disallowed paths, risk classification).
- Performance metrics (token usage, cost, timing per step).

The LSP tool (see Core Tools) also integrates with diagnostics, providing hover, go-to-definition, references, symbols, and diagnostics as tool capabilities. Diagnostic sources are extensible: cargo clippy, custom linters, code coverage tools, and workspace-specific checks can all be registered as diagnostic providers accessible through the LSP tool.

### libcorpus Integration

Norn integrates with libcorpus for code and documentation indexing. Corpus data provides context for agent steps: file summaries, symbol indices, documentation snippets, and knowledge-base entries beyond source code.

### Design System Integration

When the design-system crate exists, Norn integrates with it for structured planning, review, and documentation:

- Plans are structured JSON first, rendered Markdown second.
- Reviews are generated against design-system schemas.
- Tasks link to requirements and user stories.
- Structured outputs from agent steps map to design-system artifact types.

### Task Management

Norn provides task management tools compatible with the current Claude Code task format where useful, extended toward richer task hierarchies:

- Task creation, listing, updating, completion.
- Task hierarchies (parent/child tasks).
- Requirement linkage (task connects to design-system requirement).
- Workflow state tracking (which tasks are in which stage).

### Session Variables and String Substitution

The runtime provides a declarative, scriptable variable system. Variables can be substituted into prompts, tools, shell commands, MCP tools, hooks, and templates. Variables are not a fixed set; they are declared and populated by the runtime, profiles, and extensions.

Built-in variables include `{session_id}`, `{cwd}`, `{home}`, `{profile}`, `{role}`, `{branch}`. Additional variables are declared by profiles, workflows, or extensions as needed (e.g., `{workflow_id}`, `{step_name}`, `{task_id}`, `{workspace}`). When running inside Meridian, `{member_id}` is the session ID (agents are identified by their session).

Profiles and capabilities can include commands that populate dynamic prompt sections at runtime: current team status, active tasks, member information, workflow state. These commands execute at prompt construction time and inject their output as variable values or prompt sections.

### Hooks

Norn implements lifecycle hooks for modular extensibility:

- **Pre/post tool call**: inspect, modify, or block tool calls.
- **Pre/post LLM call**: inspect or modify the context before sending, inspect the response after receiving.
- **Session events**: session start, end, compaction, fork, branch.
- **File mutation events**: after write, after edit, after patch.
- **Diagnostic events**: after validation, after check commands.

Hook types are compatible with or inspired by Claude Code's hook system where applicable, enabling reuse of existing hook infrastructure.

---

## Provider-Specific Features

### OpenAI Responses API

The OpenAI provider implements:

- Streaming via SSE or WebSocket (as OpenAI evolves transport).
- Tool calling with function definitions.
- Structured output via `response_format` with JSON schema.
- Reasoning effort control (for reasoning models).
- Server-side web search via `ToolSpec::WebSearch` (domain filtering, content types, user location).
- Server-side image generation via `ToolSpec::ImageGeneration`.
- Session-based prompt caching.
- Model selection across the GPT-5.x family.
- Service tier selection (flex, standard, priority).

### Claude via Claude Runner

Claude integration continues through Claude Runner with:

- Anthropic prompt caching.
- Extended thinking.
- Session management (visit-based, resume).
- OAuth token pool rotation.
- Structured output via `--json-schema`.
- Profile/capability decomposition into CLI flags.

---

## Implementation Groups

The features above cluster into logical implementation groups. These are not sequential phases — many can be parallelized. They represent the natural boundaries for briefing and planning.

### Group 1: Core Runtime

The minimal agent loop that can execute a prompt, call tools, and return structured output. Everything else builds on this.

- Provider trait and OpenAI Responses API implementation (streaming, tool calling, response_format, reasoning effort, web search spec).
- Agent loop (prompt → LLM → tool calls → results → loop → schema validation → output).
- Per-event output schemas (assistant message, spoken response, tool call envelope, stop output, question, handoff, review, progress).
- Streaming event system (token-by-token response streaming, tool execution events, lifecycle events).
- Input channels (steer, follow-up, direct session input).
- Iteration control (token thresholds, time thresholds, repeated failure detection, soft handoff, continuation paths).

### Group 2: Tool Framework

The tool trait, lifecycle, and core tool implementations.

- Tool trait with pre-validate, execute, post-validate, on-success lifecycle (compile-time and runtime phases).
- Tool call envelopes with open metadata field and runtime-supplied inputs.
- Runtime-supplied tool arguments (policies injected before execution).
- Effect-based parallel scheduling.
- Bash risk classification (runtime categorization).
- Dynamic tool availability (profile/stage/location/task-based).
- Core tool implementations: Read, Write (with AST validation, read-before-overwrite, file length checks), Edit (with AST validation, blast radius, read-before-edit), ApplyPatch, Search (unified multi-mode), Bash, WebSearch, WebFetch, LSP.

### Group 3: Rules Engine

Contextual guidance system that fires based on conditions.

- Rule format (YAML front matter with trigger conditions, plain text body).
- Trigger conditions (path globs, bash/tool command matches, before/after).
- Delivery modes (system context append, context injection, message delivery).
- Lifecycle tracking (knows when rules are in/out of context, re-injects on trigger).
- Shell execution in rules for dynamic content.

### Group 4: Multi-Agent

Agent coordination, forking, messaging, and background task management.

- Agent registry (hierarchical paths, statuses, roles, no concurrency limits, RAII spawn reservation).
- Agent tools (SpawnAgent, SendMessage, WaitAgent, CloseAgent, Fork).
- Forking as a first-class primitive (context filtering, model switching, audit trail reintegration).
- Command wrappers (fork for output analysis).
- Mailbox messaging (trigger_turn flag, sequence numbers, efficient wait-for-any).
- AI-monitored background tasks (RunMonitored with lightweight monitoring model).
- Batch processing (CSV/structured fan-out).
- Goals and budgets (objective, token/time budgets, continuation policies).
- Scheduling and wake-ups (session-active cron, session-dispatched cron).

### Group 5: Context Control and Sessions

How context is managed, edited, and persisted.

- Surgical context editing (keep, suppress, summarize, inject, compact).
- Immutable audit trail (events marked, never deleted).
- Session event model (immutable append-only stream, tree-structured with IDs/parent IDs).
- Semantic quality signals (hedging detection, premature completion, circular reasoning).

### Group 6: Integration

Connecting Norn to the broader Meridian/Yggdrasil ecosystem.

- Claude Runner integration (profile-based routing, StepOutcome compatibility).
- Norn-wrapped Claude Code (strip to bare metal, tools via MCP, event capture).
- MCP client and server (consume external tools, expose Norn tools).
- Norn as MCP server mode (standalone tool exposure for any consumer).
- Rhai builtins (run_agent, spawn_agent, send_message, wait_agent, close_agent, fork_agent).
- RunScript tool (inline Rhai execution with host functions).
- Extension system (Meridian extension protocol, shared runtimes, Rust/Python/TypeScript SDKs).
- Diagnostics integration (compiler-grade reporting, extensible diagnostic providers).
- Session variables and string substitution (declarative, scriptable, shell execution in prompt templates).
- Hooks (pre/post tool call, pre/post LLM call, session events, file mutation events).

### Group 7: Session Intelligence (later)

Graph-backed memory and retrieval. Depends on Groups 1-5 being functional.

- Graph-backed sessions (Memgraph, causal queries, cross-session edges).
- DMs as high-fidelity memory (layered drill-down from DM to session to fork).
- Hybrid memory and search (Tezera, ColBERT, rerankers, graph queries).
- Workflow reflection and dream process.
- libcorpus integration (code and documentation indexing).
- Design system integration (structured JSON artifacts, plan/task/review linkage).
- Task management tools (hierarchies, requirement linkage, workflow state).
- Claude Code / Norn session translation layer.

---

## Future Considerations

### Copy-on-Write Filesystem Layers

The most significant future integration point between Norn and Yggdrasil is copy-on-write filesystem overlays as a replacement for work trees.

Current model: dispatch a workflow to a git work tree. The work tree is a full checkout with its own build directory. Multiple concurrent work trees means multiple copies of build artifacts (potentially hundreds of gigabytes). Work trees must be tracked, cleaned up, and managed.

Future model: dispatch a workflow to a CoW layer (stratum). The stratum overlays the main repository. Reads fall through to the main branch. Writes go to the stratum's upper layer. The agent sees a full repository but is actually working on a thin diff. Multiple strata can be active simultaneously, all sharing the same base filesystem and build cache.

Benefits:
- No build artifact duplication (shared build cache via symlinks or shared directory).
- No work tree management (no git worktree add/remove, no .gitignore tracking).
- LSP and diagnostics see a coherent filesystem (overlays are transparent).
- Merging operates on file-level diffs, not commit histories. Layer A changed X and Y, layer B changed Y and Z. Merge via AST merge driver on the conflicts.
- Stacking is natural: layers stack on layers. Restack by re-applying layers in order.
- Parallelization is simpler: everyone works in the main repo, different layers.

Implementation options: OverlayFS on Linux (what Docker uses), APFS cloned directories on macOS, FUSE-based overlays for portability. Practical challenges include build directory sharing, LSP compatibility testing, and integration with Yggdrasil's operation-level logging.

This is a Yggdrasil feature, not a Norn feature. Norn launches into whatever filesystem context Yggdrasil provides. But it transforms the operational model for concurrent agent work.

### Pi Extension Ecosystem Bridge

Pi extensions are TypeScript modules that register event handlers and custom tools. A Deno or TypeScript SDK extension could bridge between the Meridian extension protocol and Pi's extension API, allowing Pi-compatible extensions to run as Meridian extensions without embedding QuickJS or any JavaScript runtime in Norn. This is a future consideration, not an initial requirement.

### Rune Scripting Language

Rune is a pure-Rust scripting language with Rust-like syntax and first-class async/await. It is thematically perfect for Norn (Nordic runes). Currently pre-1.0 with known compiler bugs. Worth revisiting when it reaches a stable release — it could complement Rhai for use cases that need async scripting or a more expressive in-process language.

### Extension Components in the TUI

The terminal user interface has been built: `norn-tui` is a full interactive front-end (see Overview), assembled through the same `AgentBuilder` path as every other driver. The Meridian web view remains the primary rich visual surface. The remaining forward-looking work here is the terminal *component model* for extensions: extension components should be able to render in the TUI via a terminal component model, separate from the React-based web view components. That component bridge is not yet built.
