---
type: design
cluster: norn-runtime
title: Norn as Primary Runtime
---

# Norn as Primary Runtime

> **Historical design note (superseded for local session persistence).** This
> document records the June 2026 runtime proposal. The active local runtime now
> uses registered strict format-2 timelines under `~/.norn/session-store/` and
> the explicit offline migration contract in
> `docs/RESPONSES-API-REMEDIATION-PLAN.md`. References below to tolerant/raw
> JSONL handling or `~/.norn/sessions/` describe the earlier proposal, not the
> current persistence API.

## Intention

Norn becomes the primary runtime for all member-facing work in Meridian.
Every DM, wake prompt, inbox triage, and interactive assistant session runs
through Norn in-process within the server. Claude Runner stays only for
legacy background task dispatch until it can be retired entirely.

When this is done, member-facing sessions run as typed, observable,
in-process agent loops rather than opaque subprocess invocations. Session
state persists across visits without relying on a CLI binary's internal
persistence. The model sees a focused set of domain tools instead of
a raw shell with 175 CLI commands. The server has direct control over
agent lifecycle, resource budgets, and event routing.

This is the June 15 deadline work.

## Problem

Three problems prevent Norn from replacing Claude Runner for member-facing
work today:

**1. Session persistence is missing in-process.** The `make_norn_step_runner`
in `crates/meridian-services/src/workflow/imperative_callbacks.rs:502-518`
builds an `AgentBuilder` without calling `.session()`. This means
`build()` creates a bare `EventStore::new()` — in-memory only, no
`JsonlSink`, events dropped when the step finishes. norn-cli gets this
right: it creates an `EventStore::with_sink_and_events()` backed by a
`JsonlSink` pointing at `~/.norn/sessions/{uuid}.jsonl`. The server needs
the same durability but with server-managed paths and integration with PG
telemetry.

**2. No Meridian domain tools.** Norn currently has 16 standard tools
(read, write, edit, bash, search, patch, lsp, task, tool_search,
action_log, web_fetch, web_search, spawn, fork, signal, close). These are
developer tools. An agent running as a Meridian member needs messaging
(send DMs, read inbox), status management, workflow dispatch, VCS
operations, and project management. The Meridian CLI has 175+ commands
across 26 groups. Giving the model 175 tools would overwhelm it. The
design needs a way to expose Meridian's capabilities as a focused, layered
tool surface.

**3. No assistant service integration.** The assistant service
(`crates/meridian-services/src/assistant/service.rs`) is built around
`ClaudeProcess` — a subprocess spawned via `ClaudeCommand`, communicating
via JSONL on stdin/stdout, with a `TranslateState` converting
`ClaudeEvent` to `AssistantEvent`. Norn is in-process with a typed
`AgentEvent` broadcast channel. There is no shared abstraction between
the two paths. The session loop, event translation, persistence pipeline,
Redis streaming, WebSocket delivery, and wake prompt mechanism all assume
the subprocess model.

## Solution

### Design Principles

1. **One session model.** Norn sessions and Claude Runner sessions present
   the same `AssistantEvent` stream to consumers. PG persistence, Redis
   streaming, and WebSocket delivery work identically regardless of
   runtime.

2. **In-process over subprocess.** Norn runs as an async task in the
   server's tokio runtime, not as a spawned process. No stdin/stdout, no
   process lifecycle, no crossbeam bridges.

3. **Domain tools, not shell wrappers.** Meridian capabilities are exposed
   as native Norn tools that call service-layer functions directly.
   No HTTP round-trips, no CLI parsing.

4. **Profile-gated tool sets.** The agent profile determines which domain
   tools are available. A messaging agent gets messaging tools. A dev
   agent gets VCS tools. The model never sees tools it cannot use.

5. **Dual persistence is intentional.** JSONL files are Norn's source of
   truth for session resume. PG events are the UI's source of truth for
   history display. They serve different consumers and must not be
   conflated.

6. **Bash-free by default.** The default agent profile omits bash
   entirely. Agents operate through typed namespace tools plus file
   tools (read, write, edit, search). Bash is opt-in for profiles that
   explicitly need it (dev agents running tests). The namespace tool
   surface is the 20-30 operations agents actually need, not CLI
   completeness.

7. **Minimal surface, not CLI completeness.** Many of the 175 CLI
   commands are admin-only or redundant at higher abstraction levels.
   The namespace tools expose what agents need for their role, not a
   mechanical wrapping of every CLI subcommand.

### D1: RuntimeKind Discriminant

A `RuntimeKind` enum on `SessionHandle` distinguishes the two paths:

```
RuntimeKind::ClaudeRunner  — existing subprocess path (legacy)
RuntimeKind::Norn          — in-process Norn agent loop
```

`AssistantService::start_session()` checks configuration (profile flag,
server config, or per-session override) to select the runtime kind.
Both paths produce the same `broadcast::Sender<AssistantEvent>` channel,
so downstream consumers (persistence, streaming, WS) are runtime-agnostic.

### D2: Session Persistence Architecture

**EventStore + JsonlSink** (already proven in norn-cli) provides
write-through durability:

- `EventStore::with_sink_and_events(sink, events)` — pre-populated from
  disk for resume, new appends written through `JsonlSink`.
- `EventStore::with_sink(sink)` — empty store for new sessions, all
  appends go through `JsonlSink`.
- `EventStore::new()` — in-memory only. **Never used for member-facing
  sessions.**

**Server-managed session directory:**

```
{data_dir}/sessions/norn/
  {session_id}.jsonl     — one SessionEvent per line, append-only
  index.jsonl            — SessionIndexEntry per line
```

Where `{data_dir}` is the server's configured data directory (typically
`~/.meridian/data/`), not `~/.norn/` (which is norn-cli's local
convention). The session_id is the same UUID used in the PG
`assistant_session_events` table and the `SessionHandle` registry.

**Resume flow:**

1. `AssistantService::resume_session()` receives a session_id.
2. Server reads `{data_dir}/sessions/norn/{session_id}.jsonl`.
3. Deserialises `SessionEvent` lines into a `Vec<SessionEvent>`.
4. Creates `JsonlSink` in append mode for the same file.
5. Calls `EventStore::with_sink_and_events(sink, events)`.
6. Passes the `EventStore` to `AgentBuilder::new(provider).session(store)`.
7. Norn replays conversation history from the EventStore on `build()`.

**New session flow:** Same but with empty events and
`EventStore::with_sink(sink)`.

**Fork flow:** Copy source session's events + append a Fork marker,
write to new file, build `EventStore::with_sink_and_events()`.

### D3: Dual Persistence — JSONL and PG

Two independent persistence paths serve different consumers:

| Path | Source | Destination | Purpose |
|------|--------|-------------|---------|
| JSONL | `EventStore` write-through | `{session_id}.jsonl` | Norn conversation resume |
| PG | `AssistantEvent` broadcast | `assistant_session_events` | UI history, telemetry |

The JSONL path is owned by Norn's `EventStore`. It persists the full
conversation model: system prompts, user messages, assistant responses,
tool calls with parent-ID chaining. This is what Norn reads back on
resume.

The PG path is owned by the assistant service's `persist_events()`. It
persists `AssistantEvent` objects for the web UI's history view. This is
what the WebSocket replays on reconnect.

These are complementary, not redundant. JSONL carries Norn's internal
state (conversation threading, cache keys, usage tracking). PG carries
the UI's event stream (text deltas, tool use summaries, turn boundaries).

### D4: NornSessionLoop

The Norn equivalent of `run_session_loop()`, but async instead of
blocking:

```
async fn run_norn_session_loop(
    builder: AgentBuilder,
    session_id: Uuid,
    event_tx: broadcast::Sender<AssistantEvent>,
    command_rx: mpsc::Receiver<SessionCommand>,
    idle_timeout: Option<Duration>,
)
```

Runs as a `tokio::spawn` task (not `spawn_blocking` — Norn is async).

**Event flow:**

1. `AgentBuilder` is configured with an `AgentEventSender` (broadcast
   channel).
2. `NornTranslateState` subscribes to the `AgentEvent` broadcast and
   translates each event to `Vec<AssistantEvent>`.
3. Translated events are forwarded to the session's
   `broadcast::Sender<AssistantEvent>`.
4. The existing three consumers (PG persistence, Redis streaming, WS
   handler) receive `AssistantEvent` unchanged.

**Command handling:**

| `SessionCommand` | Norn equivalent |
|-------------------|----------------|
| `SendMessage(text)` | Enqueue as next user prompt (via a oneshot or mpsc to the agent loop) |
| `Interrupt` | Trigger `CancellationToken`, then rebuild agent for next turn |
| `Stop` | Trigger `CancellationToken`, mark session Stopped |
| `Kill` | Trigger `CancellationToken`, drop agent immediately |

**Idle timeout:** Timer resets on each `AgentEvent`. If no events within
the timeout period, stop the session.

### D5: NornTranslateState

Parallel to the existing `TranslateState` (which converts `ClaudeEvent`
→ `AssistantEvent`), `NornTranslateState` converts `AgentEvent` →
`AssistantEvent`.

The workflow path already has `translate_norn_event()` at
`workflow/persistence/norn_translate.rs:120` which converts
`ProviderEvent` → `(event_kind, payload)`. `NornTranslateState` builds
on this pattern but outputs typed `AssistantEvent` variants instead.

Key mappings:

| AgentEvent / ProviderEvent | AssistantEvent |
|----------------------------|----------------|
| `ContentDelta::Text` | `TextDelta` |
| `ContentDelta::Thinking` | `ThinkingDelta` |
| `ToolUse` (start) | `ToolUseStarted` |
| `ToolResult` | `ToolResult` |
| `Complete` (turn) | `Usage` + `TurnComplete` |
| Agent spawn/fork | `SubAgentStarted` |
| Agent result | `SubAgentCompleted` |

### D6: Hierarchical Tool Namespaces

175 flat tools would overwhelm the model. Instead, Meridian capabilities
are grouped into **namespace tools** — each namespace is a single Norn
`Tool` with a `command` discriminant.

**Namespace design (7 tools):**

| Namespace Tool | Commands | Priority |
|---------------|---------|----------|
| `meridian_messaging` | 12: send, inbox, read, search, mark_read, snooze, respond, retry, channel_send, channel_mentions, notify_check, notify_summary | Critical |
| `meridian_member` | 7: get, list, lookup, status_set, status_get, activity, set_profile | Critical |
| `meridian_workspace` | 4: workspace_list, workspace_get, workspace_members, workspace_config | Critical |
| `meridian_source` | 25: status, file_statuses, branches, log, blame, diff_file, diff_summary, tree_*, worktree_*, stage, unstage, stage_all, discard, commit, push, pull, fetch, remotes | Critical |
| `meridian_branch` | 22: branch_*, stack_*, pr_* | Critical |
| `meridian_workflow` | 14: workflow_list/show/run/status/history/cancel, scheduler_list/create/get/update/delete/enable/disable/run | Important |
| `meridian_exchange` | 21: contracts/peers/workspace lifecycle, audit, identity, send | Profile-gated |

Three separate groupings per Tom's direction:
- `meridian_member` — identity directory. Who is this person, what are they doing, what profile. Hits MemberService.
- `meridian_workspace` — workspace environment. List/get workspaces, members, config. Hits WorkspaceService.
- `meridian_exchange` — cross-instance operations. Contracts, peers, shared workspace lifecycle. Hits ExchangeService.

Admin actions redistributed: channel_list/send/search →
`meridian_messaging`, scheduler → `meridian_workflow`, workspace ops →
`meridian_workspace`, profile assignment → `meridian_member`.
`meridian_project` deferred (not set up yet). VCS renamed to Source.
Teams are not a v2 concept — no team operations.

Each namespace tool implements the Norn `Tool` trait. The `input_schema()`
returns a JSON schema with a `command` enum field plus per-command
parameter objects. The `execute()` method dispatches on the command value.

**Why namespaces, not flat tools:**

- Model sees 5-7 tools instead of 175. Within context budget.
- Each namespace has a focused description. Model can reason about
  "messaging" as a capability.
- Profile gating works at the namespace level. A messaging agent profile
  enables `MessagingTool` + `MemberTool`. A dev agent enables `SourceTool`
  + `BranchTool`.
- Adding a new CLI command to an existing group = adding a command
  variant, not a new tool.

**Why not a single uber-tool:**

- One tool with 175 commands provides no semantic signal.
- The model can't reason about "what can I do" from a single blob.
- Error messages lose context (which action failed?).

### D7: Profile-Gated Tool Sets

Agent profiles already support tool filtering via `allowed_tools` and
`disallowed_tools` in `SessionConfig`. Namespace tools integrate with
this system:

- Profile declares which namespace tools are available.
- `register_meridian_tools()` adds all namespace tools to the registry.
- `AgentBuilder::without_tools()` removes namespaces not in the profile.
- The model's tool definitions only include enabled namespaces.

**Default tool sets by agent role:**

| Role | Standard Norn Tools | Meridian Namespace Tools |
|------|--------------------|-----------------------|
| Member agent | read, write, edit, search, task | messaging, member, workflow |
| Dev agent | read, write, edit, search, patch, lsp, task, **bash** | source, branch |
| Workflow agent | read, write, edit, search, patch, task | (profile-specific) |

Bash is **opt-in only** — the dev agent profile explicitly enables it for
running tests and build commands. All other profiles omit bash. Agents
operate through typed namespace tools for Meridian operations and file
tools for code changes.

### D8: Direct Service Layer Calls

Namespace tools call Meridian service-layer functions directly, not via
HTTP. Norn runs in the same process as the services. The tool receives a
`ServiceContext` (or specific service references) through `ToolContext`.

**Injection path:**

1. `AssistantService` holds `ServiceContext` (contains PG pool, Redis,
   EventBus, all service references).
2. When building an `AgentBuilder` for a Norn session, a `CallerContext`
   (from Waffles' TA-001 brief) carrying `member_id`, `session_id`, and
   `workspace_id` is published as an `Arc` extension on `ToolContext`.
3. Service references for domain operations are also published on
   `ToolContext` extensions at agent construction time.
4. Each namespace tool extracts `CallerContext` and service references
   from the context. `CallerContext` is unforgeable — set at construction,
   not passable as tool input.

This eliminates HTTP round-trips, authentication overhead, and
serialisation costs for in-process tool calls. When Norn runs inside a
shared VM workspace (D11), tools still call in-process — against the
VM's local service layer rather than the host's. Both environments use
the same in-process pattern with different service availability.

### D9: Wake Prompt Delivery via Norn

The existing wake prompt system (`wakeup.rs`, `wake_trigger.rs`,
`wake_prompt.rs`) builds a text prompt and delivers it as
`SessionConfig::initial_message`. For Norn sessions:

1. `WakeupService` builds the wake prompt text identically.
2. `AssistantService::resume_session()` creates or resumes a Norn session.
3. The wake prompt is passed as `AgentBuilder::prompt(text)` for the
   initial turn.
4. For subsequent messages during an active session,
   `SessionCommand::SendMessage` feeds the next user turn.

The wake prompt format needs no changes — it is already plain text that
the model interprets.

### D10: Dual-Path Coexistence

Both runtimes coexist during the transition period:

**Selection logic:** Server configuration sets a default runtime kind.
Per-session override via `SessionConfig` field. Per-profile override
via profile metadata.

**Shared interfaces:** Both paths produce
`broadcast::Sender<AssistantEvent>`. Both register a `SessionHandle` in
`SessionRegistry`. Both accept `SessionCommand` via `mpsc`. All
downstream consumers are runtime-agnostic.

**Migration path:**
1. Norn becomes available as an opt-in runtime (profile flag).
2. Member-facing profiles switch to Norn by default.
3. Claude Runner reserved for legacy background dispatch.
4. Claude Runner retired when all dispatch paths are Norn-native.

### D11: Execution Context Dispatch for Namespace Tools

Namespace tools assume in-process service calls (D8). This holds on the
host Meridian server. It also holds inside shared VM workspaces
(exchange-workspaces cluster), where Norn runs inside a lightweight
virtual Meridian (workflow engine + yggdrasil + event bus + dispatch
receiver). The VM has its own service layer — just a subset (workflow +
source operations, no messaging or member services).

**Pattern:** Both host and VM provide in-process service calls. The host
`ServiceContext` has the full service set. The VM `ServiceContext` has
only the services the VM runs. Namespace tools call whichever
`ServiceContext` they receive — same code, same in-process path, different
service availability.

When a tool calls an unavailable service (e.g. `meridian_messaging.send`
in a VM that only has workflow + source), the service interface returns a
context-aware error: "messaging is not available in shared workspace
mode — use your home workspace for messaging operations." The error must
be informative enough that the agent understands the constraint and can
adjust.

The former HTTP-proxy backend (credential proxy) is no longer needed.
Credentials are injected per-dispatch via encrypted envelopes (X25519 +
AES-256-GCM), decrypted in-process, used, and zeroed. No proxy.

### D12: Exchange and Certificate Integration

When agents run in shared VM workspaces, the Norn session model must
account for agent certificates. `CallerContext` (from TA-001) gains an
optional `agent_cert_fingerprint: Option<[u8; 32]>` field. When present,
namespace tool actions that cross the exchange boundary sign with the
agent cert. When absent, instance-level signing applies.

Certificate and Merkle primitives live in the `meridian-trust` crate
(extracted from `meridian-exchange`). NR-006's `CallerContext` depends on
`meridian-trust` for cert verification, not on `meridian-exchange`.

The `meridian_exchange` namespace tool exposes contract management, peer
operations, audit queries, and workspace provisioning. It is gated by
profile — only agents participating in exchange work have it enabled.

### D13: Workspace Focus for Shared Workspace Access

Agents on the home instance need to access shared VM workspace files.
The virtiofs mount provides the access path. The `meridian_source`
tool's working directory resolution supports a **workspace focus**
concept: when an agent session is focused on a shared workspace, source
operations (status, diff, commit, etc.) target the shared workspace's
files through the virtiofs mount path rather than the home instance's
local repository.

The agent doesn't switch execution context — it still runs locally with
full services (messaging, member, etc.) — but its source operations
transparently route to the focused workspace's files. The focus is set
via session metadata (workspace_id on CallerContext combined with
workspace mode from WorkspaceService).

The full workspace focus mechanism is specified in exchange-workspaces
D20 (workspace_focus/workspace_unfocus commands on `meridian_exchange`,
mount path resolution, session scoping). This decision defers to D20
for the authoritative specification. The norn-runtime implication is
that `meridian_source` working directory resolution must support
virtiofs mount paths when the session's workspace focus is active.

### D14: Fix make_norn_step_runner Persistence

Separate from the assistant integration but required for correctness:
`make_norn_step_runner` must call `.session(store)` on the
`AgentBuilder`. The session file path is derived from the workflow
execution step context: `{data_dir}/sessions/workflow/{step_id}.jsonl`.

This fix is a specific brief (not the same as the assistant integration
briefs) because it touches the workflow path, not the assistant path.

## Goals

1. Member-facing sessions run through Norn in-process with full session
   persistence and resume capability.
2. The model sees 5-7 focused namespace tools for Meridian operations,
   not 175 flat commands.
3. PG persistence, Redis streaming, and WebSocket delivery work
   identically for Norn and Claude Runner sessions.
4. Wake prompts and inbox delivery work through Norn without changes to
   the wake prompt format.
5. Agent profiles control which namespace tools are available, matching
   the agent's role.
6. The transition is incremental — both runtimes coexist until Claude
   Runner can be retired.

## Non-Goals

- **Retiring Claude Runner.** This cluster wires Norn in alongside Claude
  Runner. Retirement is a separate, future decision.
- **Changing the wake prompt format.** The existing format works. The
  delivery mechanism changes, not the content.
- **MCP server integration.** Norn tools are in-process. MCP is a
  separate integration surface.
- **Norn TUI for server sessions.** Server sessions are headless. The
  TUI is for norn-cli interactive use.
- **Migrating existing Claude Runner session history to Norn format.**
  New sessions use Norn. Existing sessions remain in Claude Runner
  format.
- **Real-time tool permission prompting.** Server sessions run in headless
  mode (no human in the loop for tool permissions). Permission is
  profile-configured.

## Structure

```
crates/meridian-services/src/
├── assistant/
│   ├── mod.rs                 — pub mod + pub use (existing)
│   ├── service.rs             — AssistantService with RuntimeKind branching (modify)
│   ├── types.rs               — RuntimeKind enum, SessionConfig additions (modify)
│   ├── registry.rs            — SessionHandle with RuntimeKind field (modify)
│   ├── session_loop.rs        — existing Claude Runner loop (unchanged)
│   ├── norn_session_loop.rs   — async Norn session loop (NR-003)
│   ├── translate.rs           — existing TranslateState (unchanged)
│   ├── norn_translate.rs      — NornTranslateState: AgentEvent → AssistantEvent (NR-003)
│   ├── norn_session_store.rs  — server-side session file management (NR-002)
│   ├── events.rs              — AssistantEvent (unchanged)
│   ├── persistence.rs         — persist_events (unchanged, runtime-agnostic)
│   ├── streaming.rs           — stream_deltas_to_redis (unchanged)
│   ├── wake_prompt.rs         — wake prompt construction (unchanged)
│   ├── wakeup.rs              — WakeupService (minor: resume_session path)
│   └── ...                    — remaining files unchanged
│
├── workflow/
│   ├── imperative_callbacks.rs — make_norn_step_runner persistence fix (NR-002)
│   └── ...
│
crates/meridian-tools/src/
├── mod.rs                     — pub mod + register_meridian_tools() (NR-006)
├── context.rs                 — MeridianToolContext + CallerContext (NR-006)
├── messaging.rs               — messaging namespace tool (NR-006)
├── member.rs                  — member namespace tool (NR-006)
├── workspace.rs               — workspace namespace tool (NR-006)
├── source.rs                  — source control namespace tool (NR-007)
├── branch.rs                  — branch namespace tool (NR-007)
├── workflow.rs                — workflow namespace tool (NR-007)
└── exchange.rs                — exchange namespace tool (future)

crates/norn/src/
│
├── tool/
│   └── traits.rs              — Tool trait, ToolCategory (unchanged, generic)
│
├── session/
│   └── store.rs               — EventStore, JsonlSink (unchanged)
│
├── agent/
│   └── builder.rs             — AgentBuilder (unchanged API)
```

Namespace tool implementations live in `crates/meridian-tools/src/`
(not inside the norn crate) because they depend on Meridian-specific service
types (`ServiceContext`, `MessagingService`, etc.). The Norn crate stays
domain-agnostic. Tools are injected into `AgentBuilder` via `.tool()` at
session construction time, same pattern as workflow provider injection.

`CallerContext` (defined in Waffles' TA-001 brief) carries `member_id`,
`session_id`, `workspace_id`, and `agent_cert_fingerprint` (per D12).
Published as an `Arc` extension on `ToolContext` at agent construction
time. Unforgeable — not passable as tool input. All namespace tools
extract `CallerContext` from the `ToolContext` extension map.

Naming convention: `meridian_` prefix on all namespace tools to
distinguish them from standard Norn tools in the tool registry.

## Current Inventory

### Assistant Service (crates/meridian-services/src/assistant/)

| File | Lines | Role |
|------|-------|------|
| service.rs | ~300 | Gateway: token acquisition, command building, session spawn |
| session_loop.rs | ~250 | Claude Runner loop: crossbeam select, ProcessReader/Writer |
| translate.rs | ~350 | TranslateState: ClaudeEvent → AssistantEvent |
| events.rs | ~200 | AssistantEvent enum (21 variants) |
| types.rs | ~310 | SessionConfig, SessionStatus, SessionCommand |
| registry.rs | ~150 | SessionRegistry: HashMap<String, SessionHandle> |
| persistence.rs | ~120 | persist_events: broadcast subscriber → PG |
| streaming.rs | ~100 | stream_deltas_to_redis: broadcast subscriber → Redis |
| wakeup.rs | ~200 | WakeupService: poll + event-triggered wake |
| wake_prompt.rs | ~150 | Wake prompt text construction |
| wake_trigger.rs | ~120 | Event-driven wake on DM/mention |

### Norn Session System (crates/norn/src/session/)

| File | Role |
|------|------|
| store.rs | EventStore (in-memory + optional JsonlSink write-through) |

### Norn Tool System (crates/norn/src/tools/)

16 standard tools registered via `register_standard_tools()` in
`registry_builder.rs`. Tool trait at `tool/traits.rs` with 11
`ToolCategory` variants.

### Existing Norn Integration (workflow path)

| File | Role |
|------|------|
| imperative_callbacks.rs:418 | make_norn_step_runner — AgentBuilder without .session() |
| persistence/norn_translate.rs | bridge_norn_events: ProviderEvent → execution_step_events PG |
| persistence/batcher.rs | Batched PG insert for step events |

## Constraints

- **CO1**: No new external dependencies. Norn tools call service-layer
  functions via `ServiceContext`, not HTTP.
- **CO2**: `AssistantEvent` is the single event type for all downstream
  consumers. Both runtimes produce it.
- **CO3**: JSONL session files are Norn's source of truth for resume.
  PG is the UI's source of truth. Neither replaces the other.
- **CO4**: The model sees at most 25 tools (16 standard Norn + up to 7
  namespace + 2 optional web). Profile filtering reduces this in practice.
- **CO5**: Session IDs are UUIDs shared between the JSONL filename, PG
  `assistant_session_events` key, and `SessionHandle` registry key.
- **CO6**: Wake prompt delivery mechanism changes, format does not.
- **CO7**: `mod.rs` contains only `pub mod` declarations and re-exports.
- **CO8**: No file over 500 lines of code (excluding tests, comments,
  whitespace).
- **CO9**: All errors handled via `thiserror` in library code. No
  `.unwrap()` or `.expect()` in library code.
- **CO10**: Namespace tools implement the existing `Tool` trait. No
  changes to the trait interface.
