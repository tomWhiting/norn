---
type: design
cluster: norn-agents
title: "Norn Agents: Sub-Agent Lifecycle, Profiles, and Hierarchical Tasks"
---

# Norn Agents: Sub-Agent Lifecycle, Profiles, and Hierarchical Tasks

## Intention

When this work is done, a Norn agent can spawn sub-agents that are real agents — configured through profiles, visible through hierarchical tasks, steerable through inbound channels, and observable through per-event progress schemas. The parent does not block. The child does not run naked. The coordination surface is the shared infrastructure that Norn already provides: tool boundaries for synchronisation, ToolContext extensions for shared state, and per-tool lifecycle callbacks for observation.

The experience should feel like managing a team: assign work with clear role definitions, check progress without interrupting, redirect when priorities change, and collect structured results when the work is done.

## Problem

Norn has 17 registered tools. Seven of them do not work:

- **Five agent tools** (spawn_agent, fork, send_message, wait_agent, close_agent) all error immediately because AgentToolInfra is never installed on the ToolContext. The wiring code does not exist in norn-cli.
- **task** errors because neither SharedTaskStore nor InMemoryTaskStore is installed on the ToolContext.
- **tool_search** errors because SharedToolCatalog is never installed on the ToolContext.

Beyond the wiring gaps, the agent tools have deeper problems even if AgentToolInfra were installed:

- **spawn_agent** passes `tools: &[]` to the child — the model sees no tool definitions.
- **spawn_agent** creates a bare `LoopContext("You are a sub-agent.")` — no profile, no system instructions, no reasoning config, no diagnostics.
- **spawn_agent** is synchronous — blocks the parent until the child completes.
- **wait_agent** polls the registry every 50ms instead of using the watch channels that already exist on the mailbox.
- **No profile resolution** is available from within libnorn. Profile loading is CLI-only. Sub-agents cannot load a profile by name.
- **Tasks are in-memory only** — vanish when the session ends. No persistence.
- **Tasks are flat** — no parent/child hierarchy, no agent ownership, no status roll-up.
- **No completion notification** — parent has no way to know a child finished except polling.
- **No progress visibility** — parent cannot peek at what a child is doing mid-execution.

Profiles in norn-cli use TOML/JSON at `~/.norn/config/profiles/`. This is inconsistent with the Meridian profile format (markdown with YAML frontmatter) and the 31 existing profiles at `.meridian/profiles/`.

## Solution

Three work streams, each independently deliverable.

### Group 1: Profile System Overhaul

#### D1: Markdown profiles with YAML frontmatter

Norn profiles adopt the same format as claude-runner and Meridian: markdown files where YAML frontmatter provides structured configuration and the body becomes the system prompt. The existing 31 profiles at `.meridian/profiles/` demonstrate the format — `name`, `description`, `model`, `tools`, `disallowedTools`, with the markdown body as the system prompt.

The `claude-runner` crate already has the full parser (`capabilities/parser.rs`) and scanner (`capabilities/scanner.rs`) implementation. Norn's profile loader should reuse this parsing approach rather than reimplementing it.

#### D2: Profile directory at `~/.norn/profiles/`

Profiles move from `~/.norn/config/profiles/` to `~/.norn/profiles/`. Capabilities (composable fragments that layer onto profiles) live at `~/.norn/capabilities/`. This aligns with the flat `~/.norn/` directory structure used by sessions, debug, and tasks.

#### D3: Two-tier profile scanning

Profile resolution follows the claude-runner Scanner pattern: workspace-level first (`{cwd}/.norn/profiles/`), then user-level (`~/.norn/profiles/`). Workspace profiles win on name collision. This allows per-project profile customisation. The `.meridian/profiles/` directory should also be checked as an additional tier for Meridian-integrated workspaces.

#### D4: Profile resolution in libnorn

Profile resolution moves from norn-cli into libnorn so that SpawnAgentTool and ForkTool can load profiles by name at runtime. A new `norn::profile::loader` module implements the scanner and frontmatter parser. The CLI delegates to this module instead of its own `resolve_profile`.

#### D5: Spawn with profile

SpawnAgentTool accepts an optional `profile` parameter (a profile name resolved through the scanner). When provided, the child agent's LoopContext is built through `from_profile()` using the resolved profile, inheriting system instructions, tool allow-lists, reasoning config, prompt commands, and capabilities. When omitted, the child inherits a minimal default profile.

### Group 2: Persistent Hierarchical Task Store

#### D6: Disk-backed task store with named task groups

Tasks persist to `~/.norn/tasks/{group-slug}/` as individual JSON files (`{task-id}.json`). The group slug is a human-readable name (e.g. `implement-hooks`, `norn-agents-wiring`) set by the creating agent or auto-generated from the first task description. Task groups are session-agnostic — they survive across sessions, allowing multi-session work and cross-session handoffs.

A `DiskTaskStore` implements the existing `TaskStore` trait, reading and writing through the filesystem. The `InMemoryTaskStore` remains available for testing and ephemeral sessions.

The task group name is the coordination key: the parent passes it to sub-agents so they all read and write the same task tree. The TaskTool gains `create_group` and `list_groups` actions alongside the existing CRUD operations.

#### D7: Hierarchical tasks

TaskEntry gains two fields:

- `parent_task_id: Option<String>` — links a subtask to its parent, forming a tree.
- `assigned_agent: Option<String>` — records which agent (by registry path) is assigned to this task.

New operations: `create_subtask` (creates with parent link), `children(parent_id)` (list direct children), `ancestors(task_id)` (walk to root). Status roll-up: a parent task's effective status reflects its children — Blocked if any child is Blocked, InProgress if any child is InProgress, Completed only when all children are Completed.

#### D8: Atomic task claiming

An agent claims a task via `claim(task_id, agent_path)`. The claim succeeds only if the task has no current `assigned_agent`. This prevents duplicate work when multiple agents operate on the same task tree.

### Group 3: Agent Tool Wiring and Sub-Agent Lifecycle

#### D9: ToolContext extension wiring

Three extensions are installed on the shared ToolContext during `build_runtime`:

- `AgentToolInfra` — enables the five agent tools.
- `SharedTaskStore` — enables the task tool. Uses `DiskTaskStore` with the session's task directory.
- `SharedToolCatalog` — enables the tool_search tool. Built from the registry's tool definitions.

#### D10: Async spawn with AgentHandle

SpawnAgentTool launches the child via `tokio::spawn` and returns immediately with an `AgentHandle` containing the child's agent_id, registry path, and task_id. The parent model continues working while the child executes. The handle is stored as a ToolContext extension keyed by agent_id.

The AgentHandle provides:

- A `tokio::sync::watch::Sender<AgentStatus>` updated by the child on status transitions.
- An `InboundSender` for the parent to push Steer and FollowUp messages.

#### D11: Reactive wait

WaitAgentTool subscribes to the child's status watch channel instead of polling. Zero CPU when idle. Supports `tokio::select!` across multiple children for first-to-finish patterns.

#### D12: Filtered tool definitions for children

When spawning a child, the SubAgentExecutor's allow-list is used to filter the parent registry's tool definitions. The filtered definitions are passed to `run_agent_step` as the `tools` parameter so the child model can see and call them.

#### D13: Per-event Progress schema for sub-agents

Sub-agents are configured with an `EventType::Progress` schema via the existing `EventSchemaSet` mechanism. The schema enforces structured status updates that the parent can read from the child's EventStore. Combined with `tool_use_description` on every tool call envelope, the parent has two channels of visibility into child progress without the child needing to explicitly report.

#### D14: Completion notification via mailbox

When a child reaches a terminal status (Completed or Failed), it sends a message to the parent's mailbox with `trigger_turn: true`. This wakes the parent at its next tool boundary, allowing reactive response to child completion.

#### D15: Recursive agent tree shutdown

`close_agent` on a parent agent performs a DFS shutdown of all descendants. Each child transitions to Completing, and its children are shut down recursively before it transitions to its terminal state.

#### D16: InboundChannel wiring for sub-agents

SpawnAgentTool creates an `inbound_channel()` for the child. The parent holds the `InboundSender` (via the AgentHandle) and the child receives the `InboundChannel`. The parent can push Steer messages (delivered at the next tool boundary) and FollowUp messages (delivered when the child would otherwise stop). This uses the existing InboundChannel infrastructure that already works for the top-level agent.

## Goals

1. All 17 registered tools are functional — zero tools error due to missing ToolContext extensions.
2. Sub-agents are configured through profiles with full system instructions, tool access, and reasoning config.
3. Profile format is markdown with YAML frontmatter, compatible with the existing 31 Meridian profiles.
4. Tasks persist to disk across sessions and support parent/child hierarchies with status roll-up.
5. Spawn is asynchronous — the parent continues working while the child executes.
6. Wait is reactive — zero-CPU idle wait using watch channels, not polling.
7. The parent can observe child progress through per-event Progress schemas and tool_use_descriptions.

## Non-Goals

- **Multi-provider sub-agents.** Children use the same provider as the parent. Cross-provider spawning is a future concern.
- **Agent persistence and resume.** Agents are in-memory for their session lifetime. Persisting agent state for cross-session resume is out of scope.
- **Meridian collective messaging bridge.** Integration between Norn's mailbox and Meridian's DM/channel system is a separate cluster.
- **Dynamic tool availability.** Adding or removing tools from a running agent mid-session is not addressed.
- **Agent nicknames or cosmetic identity.** Not essential for the core lifecycle.
- **HookRegistry-based observation.** Sub-agent observation uses per-tool lifecycle callbacks (pre_validate, post_validate, on_success) and per-event schemas, not the HookRegistry.

## Structure

```
crates/norn/
├── src/
│   ├── profile/
│   │   ├── mod.rs              — pub mod + re-exports (exists, extended)
│   │   ├── types.rs            — Profile, Capability structs (exists, needs markdown fields)
│   │   ├── loader.rs           — NEW: Scanner, parse_profile, frontmatter parsing
│   │   └── resolve.rs          — NEW: capability resolution (merge into profile)
│   ├── tools/
│   │   ├── task.rs             — TaskTool, TaskStore trait, TaskEntry (exists, needs hierarchy)
│   │   ├── task_disk.rs        — NEW: DiskTaskStore implementation
│   │   └── agent/
│   │       ├── spawn.rs        — SpawnAgentTool (exists, needs profile + async + tool defs)
│   │       ├── coord.rs        — WaitAgent, CloseAgent (exists, needs reactive wait + tree shutdown)
│   │       ├── handle.rs       — NEW: AgentHandle with status watch + inbound sender
│   │       └── infra.rs        — AgentToolInfra (exists, needs wiring in CLI)
│   └── ...

crates/norn-cli/
├── src/
│   ├── config/
│   │   ├── profile_loader.rs   — MODIFY: delegate to norn::profile::loader
│   │   └── paths.rs            — MODIFY: profiles_dir() → ~/.norn/profiles/
│   ├── runtime/
│   │   ├── builder.rs          — MODIFY: wire AgentToolInfra, SharedTaskStore, SharedToolCatalog
│   │   └── wiring.rs           — exists, extension wiring added to build_runtime
│   └── ...
```

## Current Inventory

### Existing Meridian Profiles (31 files at `.meridian/profiles/`)

architect, bash-guy, code-reviewer, codebase-explorer, convention-reviewer, coordinator, criterion-reviewer, crushed-developer, debugger, designer, developer, documenter, gleam-developer, gleam-hardener, hardener, messenger, planner, researcher, review-lead, review-synthesizer, and 11 more. All in markdown+YAML frontmatter format.

### Existing libnorn Infrastructure

| Component | File | Status |
|-----------|------|--------|
| AgentRegistry | `agent/registry.rs` | Complete |
| Mailbox | `agent/mailbox.rs` | Complete — watch channels, sequence numbers |
| Fork | `agent/fork.rs` | Complete |
| Monitor | `agent/monitor.rs` | Complete |
| Goals | `agent/goals.rs` | Complete |
| SpawnAgentTool | `tools/agent/spawn.rs` | Broken — bare LoopContext, no tools, synchronous |
| Coordination tools | `tools/agent/coord.rs` | Exists — WaitAgent polls, CloseAgent not recursive |
| AgentToolInfra | `tools/agent/infra.rs` | Exists — never wired |
| TaskTool | `tools/task.rs` | Exists — in-memory only, flat, never wired |
| ToolSearchTool | `tools/tool_search.rs` | Exists — never wired |
| InboundChannel | `loop/inbound.rs` | Complete |
| EventSchemaSet | `loop/event_schemas.rs` | Complete — includes EventType::Progress |
| claude-runner parser | `claude-runner/src/capabilities/parser.rs` | Complete — reference implementation |
| claude-runner scanner | `claude-runner/src/capabilities/scanner.rs` | Complete — reference implementation |

### norn-cli Extension Wiring

| Extension | Status |
|-----------|--------|
| DiagnosticCollector | Installed |
| AgentToolInfra | NOT installed |
| SharedTaskStore | NOT installed |
| SharedToolCatalog | NOT installed |
| SkillSearchPaths | NOT installed (tool not registered) |

## Constraints

- CO1: No hardcoded limits on agent count, task count, or task depth (per norn DESIGN.md D8).
- CO2: No `.unwrap()` or `.expect()` in library code.
- CO3: Profile format must be markdown with YAML frontmatter, compatible with the 31 existing Meridian profiles.
- CO4: Tasks persist to `~/.norn/tasks/{group-slug}/` — filesystem only, no database, session-agnostic.
- CO5: All files under 500 lines of code (excluding tests, comments, whitespace).
- CO6: Profile resolution must work from libnorn so tools can load profiles at runtime.
- CO7: Watch-channel-based reactive wait, not polling.
- CO8: Append-only session events (per norn DESIGN.md D11).
- CO9: Profiles at `~/.norn/profiles/`, not `~/.norn/config/profiles/`.
