---
type: design
cluster: norn-hooks
title: "Norn Hooks: Config-Driven Lifecycle Hook System"
---

# Norn Hooks: Config-Driven Lifecycle Hook System

## Intention

When this work is done, any Norn deployment can declare lifecycle hooks in a settings file and have them enforced at runtime — no Rust code required. A shell script that validates tool arguments before execution, a logger that records every LLM call, a gate that prevents the agent from stopping prematurely — all expressible as JSON config entries that reference external commands.

The experience should feel like writing a CI pipeline: declare what runs, when, and what happens if it fails. The hook system is invisible when unconfigured and predictable when active. Claude Code users moving to Norn should recognise the hook model immediately — same event taxonomy, same shell protocol, same exit-code semantics.

The existing trait-based hooks are the foundation. Config-driven hooks are a convenience layer that produces trait implementations at startup. Both coexist in the same registry, share the same dispatch semantics, and are indistinguishable at execution time.

## Problem

Norn has a well-tested hook infrastructure — five async traits covering pre/post tool, pre/post LLM, and session events, with a HookRegistry that dispatches them at the right points in the agent loop. But this infrastructure is code-only: every hook must be a Rust struct implementing a trait, compiled into the binary.

This means:

- **No user-configurable hooks.** Users cannot add a pre-tool validation script, a post-tool logging hook, or a stop gate without writing Rust and recompiling.
- **No settings-driven behaviour.** Claude Code users configure hooks in settings.json with shell commands and matchers. Norn has no equivalent.
- **Limited hook surface.** The five existing traits cover tool and LLM boundaries plus session events, but miss user prompt submission, agent stop, sub-agent lifecycle, session lifecycle, and compaction — all of which Claude Code hooks support.
- **No input modification.** Pre-tool hooks can block execution but cannot modify tool arguments. Claude Code's PreToolUse hooks can rewrite arguments before execution.
- **No matching.** All pre-tool hooks fire for every tool. There is no way to target hooks at specific tools by name pattern.

## Solution

### Design Principles

1. **Extend, don't replace.** The existing trait system is the execution engine. Config-driven hooks produce trait implementations. No new dispatch path.
2. **Settings define operational behaviour, not capability.** Hooks live in settings files, not on profiles. Profiles define what an agent can do (model, tools, instructions). Settings define how the runtime behaves around the agent's actions.
3. **Sequential dispatch, first-Block-wins.** Registration order determines priority. No parallel pre-hook execution. This is the existing documented and tested contract.
4. **Shell hooks are external observers.** They add latency, may fail, and have unknown runtime characteristics. The design accounts for this at every point: timeouts are required, session event shell hooks are fire-and-forget, and parse failures degrade gracefully.

### Group 1: Module Promotion and Trait Extensions

#### D1: Promote hooks.rs to hooks/ folder module

The current `integration/hooks.rs` (493 lines) is approaching the 500-line limit and will exceed it when new traits are added. Promote to `integration/hooks/` with the existing code becoming `traits.rs` and new files for config, shell execution, matchers, and I/O protocol.

#### D2: HookOutcome gains a Modify variant

`HookOutcome::Block { reason }` becomes one of three outcomes. The new `Modify` variant carries an updated tool input value. Only `PreToolHook` uses `Modify` — all other pre-hooks see `Proceed` or `Block` only.

The modification point in the dispatch chain is between the pre-hook return and the `execute_single_tool` call in `tool_dispatch.rs`. When `Modify` is returned:

1. The envelope's `model_args` are replaced with the modified value (full replacement, not merge).
2. The modified args are serialized and passed to `execute_single_tool` instead of the original `tc.arguments`.
3. `pre_validate` sees the modified args — it validates what the tool will actually receive.
4. `post_validate` and `on_success` also see the modified args.

Runtime inputs are not modifiable by hooks.

#### D3: Audit trail for hook modifications

When a pre-tool hook modifies input, the modification must be recorded for debugging. A `HookModification` entry records the hook source, original args, and modified args. This entry is attached to the tool call's metadata in the EventStore. Not required for Phase 1 but the data path must not be closed off — the envelope and tool result recording should accommodate an optional modification record.

#### D4: HookType enum expansion

The `HookType` enum in `error.rs` currently has only `PreLlm`. Expand it to enumerate all hook categories that can block: `PreTool`, `PreLlm`, `UserPrompt`, `Stop`, `SubagentStop`, `PreCompaction`. Better error messages for hook blocks.

#### D5: New hook traits for missing event types

Six new async traits covering the events Norn currently has no hooks for:

- **UserPromptHook** — fires when the user (or orchestrator) submits a prompt, before it enters the agent loop. Can block (reject the prompt) or proceed. Dispatch point: the entry to `run_agent_step`, before initial message construction.
- **StopHook** — fires when the model would stop (no more tool calls, final text produced). Can block (force the loop to continue with an injected reason) or proceed. Dispatch point: after the final iteration, before `AgentStepResult` is returned.
- **SubagentHook** — fires on sub-agent spawn (start) and completion (stop). Start is observational. Stop can block (prevent the agent from being marked as completed, forcing it to continue). Dispatch point: `spawn.rs` launch and finish functions.
- **SessionLifecycleHook** — fires on session start and session end. Observational only (no blocking). Dispatch point: session construction and teardown in the CLI runtime builder.
- **CompactionHook** — fires before auto-compaction triggers. Can block (prevent compaction). Dispatch point: `compaction.rs` before the compaction event is appended.
- **PostToolFailureHook** — fires after a tool execution fails (distinct from PostToolHook which fires on success). Observational only. Dispatch point: `tool_dispatch.rs` when the tool returns an error output.

All six follow the same pattern as the existing five: async trait, `Send + Sync`, registered on `HookRegistry` via a `Hook` enum variant.

### Group 2: Config Schema and Settings Loading

#### D6: Hook event type taxonomy

A closed enum maps config event names to trait dispatch. The names follow The Count's snake_case convention matching the trait names:

| Config Name | Trait | Can Block? |
|:------------|:------|:-----------|
| `pre_tool` | PreToolHook | Yes (Block or Modify) |
| `post_tool` | PostToolHook | No |
| `post_tool_failure` | PostToolFailureHook | No |
| `pre_llm` | PreLlmHook | Yes |
| `post_llm` | PostLlmHook | No |
| `session_event` | SessionEventHook | No |
| `user_prompt` | UserPromptHook | Yes |
| `stop` | StopHook | Yes |
| `subagent_start` | SubagentHook (start) | No |
| `subagent_stop` | SubagentHook (stop) | Yes |
| `session_start` | SessionLifecycleHook (start) | No |
| `session_end` | SessionLifecycleHook (end) | No |
| `pre_compaction` | CompactionHook | Yes |

#### D7: Hook configuration structure

Each event type maps to an array of hook groups. Each group has an optional matcher and an array of hook commands. Aligns with The Count's config schema.

A hook group contains:

- **matcher** (optional) — a regex pattern tested against the matcher input for that event type. For tool events, the matcher input is the tool name. For subagent events, it is the agent type or profile name. When absent or empty, the hook matches all events of its type.
- **hooks** — an array of hook command entries. Each entry specifies a command string and a required timeout in seconds.

#### D8: Settings loading with 3-tier merge

Settings are parsed from three locations in ascending priority:

1. `~/.norn/settings.json` — user-global settings
2. `.norn/settings.json` — project settings (intended for version control)
3. `.norn/settings.local.json` — local project overrides (not version controlled)

The typed merge operation concatenates hook groups in priority order, but the
runtime trust boundary rejects every non-empty hook slot from both project
layers before merge or registration. Only user-global settings and
programmatic construction currently grant shell-hook execution authority.
Supporting repository hooks requires a separate provenance-preserving consent
design; a cloned settings file and ordinary precedence do not constitute
consent.

Settings are captured at startup. Runtime modifications to settings files have no effect until the next session.

#### D9: No hardcoded timeout default

Per Tom's edict: no assumed defaults. The timeout field on every hook command entry is required. The settings schema enforces this. If a future default is desired, it will be a top-level `default_hook_timeout` value in settings, set by Tom.

### Group 3: Shell Command Execution

#### D10: ShellCommandHook — the config-to-trait bridge

A single struct that implements all hook traits. Constructed from a parsed config entry at startup. At dispatch time:

1. Checks the matcher against the event's matcher input. If no match, returns `Proceed`.
2. Serializes a `HookInput` JSON object to the child process's stdin.
3. Spawns `sh -c <command>` with environment variables set.
4. Waits for completion with the configured timeout.
5. Interprets the result via the exit code protocol.

One `ShellCommandHook` instance is created per hook command entry in the config. Multiple instances may share the same event type and matcher.

#### D11: Exit code protocol

Mirrors Claude Code exactly:

- **Exit 0** — success. Parse stdout as JSON `HookOutput`. Map fields to `HookOutcome`.
- **Exit 2** — blocking error. `HookOutcome::Block` with stderr as the reason.
- **Any other exit code** — non-blocking error. Log stderr at `warn` level. Return `HookOutcome::Proceed`.

When exit 0 stdout is empty or not valid JSON, treat as a plain success with no special output. Return `HookOutcome::Proceed`.

#### D12: HookInput — JSON passed to stdin

A serializable struct containing all context the hook command needs. Fields vary by event type, but common fields are always present:

- `session_id` — current session identifier
- `cwd` — working directory
- `hook_event_name` — the event type name (e.g. `pre_tool`)
- `agent_id` — current agent identifier
- `profile_name` — profile name if any

Tool-specific fields (present for pre_tool, post_tool, post_tool_failure):
- `tool_name` — name of the tool
- `tool_input` — the tool's arguments as a JSON value
- `tool_call_id` — provider-assigned tool call identifier

Post-tool additionally includes:
- `tool_output` — the tool's output as a JSON value
- `tool_duration_ms` — execution duration
- `tool_is_error` — whether the output represents an error

LLM-specific fields (present for pre_llm, post_llm):
- `model` — model identifier
- `message_count` — number of messages in the request

Stop-specific fields:
- `final_text` — the model's final text output

Subagent-specific fields:
- `subagent_id` — the child agent's identifier
- `subagent_type` — the child's profile name or agent type

#### D13: HookOutput — JSON parsed from stdout

A deserializable struct for exit-0 output. Fields vary by event type:

Common fields (all event types):
- `reason` — explanation string (used with `decision: "block"`)

Pre-tool specific:
- `decision` — `"proceed"`, `"block"`, or `"modify"` (default: `"proceed"`)
- `updated_input` — replacement tool args (required when decision is `"modify"`)
- `additional_context` — text injected as a dynamic system section

Other blocking events (pre_llm, user_prompt, stop, subagent_stop, pre_compaction):
- `decision` — `"proceed"` or `"block"` (default: `"proceed"`)

Non-blocking events: stdout is ignored (hook is observational).

#### D14: Environment variables for shell hooks

Standard env vars set on every shell hook process:

- `NORN_PROJECT_DIR` — working directory
- `NORN_SESSION_ID` — current session identifier
- `NORN_AGENT_ID` — current agent identifier
- `NORN_PROFILE` — profile name (empty string if no profile)
- `NORN_HOOK_EVENT` — event type name

#### D15: Session event shell hooks are fire-and-forget

Shell command hooks registered for `session_event` fire via `tokio::spawn` with timeout. They do not block the agent loop. Trait-based `SessionEventHook` implementations remain synchronous (awaited in dispatch order). The rationale: session event hooks fire on every `store.append()` — a slow shell hook would bottleneck the entire loop. Trait hooks are internal and expected to be fast.

#### D16: Non-reentrant session event dispatch

A runtime guard on `HookRegistry` prevents recursive dispatch of session event hooks. If a `SessionEventHook` implementation appends to the event store (which calls `append_and_notify`, which fires hooks), the inner dispatch is skipped. This prevents infinite recursion without requiring a documented-but-unenforceable contract.

### Group 4: Matchers

#### D17: Regex-based hook matchers

Matchers are compiled regex patterns. The matcher input depends on the event type:

| Event Type | Matcher Input |
|:-----------|:-------------|
| `pre_tool`, `post_tool`, `post_tool_failure` | Tool name |
| `pre_llm`, `post_llm` | Model name |
| `subagent_start`, `subagent_stop` | Profile name or agent type |
| `user_prompt` | (no matcher — always fires) |
| `stop` | (no matcher — always fires) |
| `session_start`, `session_end` | (no matcher — always fires) |
| `session_event` | Event variant name (e.g. `UserMessage`, `ToolResult`) |
| `pre_compaction` | (no matcher — always fires) |

When no matcher is specified, the hook fires for all events of its type. The empty string and `*` are treated as match-all for consistency with Claude Code.

Regex compilation happens at settings load time (startup). Invalid regex patterns are rejected with a config error.

## Goals

1. Shell command hooks configurable in settings.json are dispatched at all 13 event types with correct exit-code semantics.
2. Pre-tool hooks can modify tool arguments via `HookOutcome::Modify`, with the modified args flowing through pre_validate, execute, and post_validate.
3. All existing trait-based hook tests pass unchanged after module promotion.
4. Regex-based matchers filter hook execution by tool name, model name, or event variant.
5. Settings loaded from three tiers with correct merge precedence, while
   project/local hook commands fail closed before registration.
6. Sequential dispatch with first-Block-wins is preserved for all pre-hooks.
7. Session event shell hooks are fire-and-forget and do not block the agent loop.

## Non-Goals

- **Permission system.** Claude Code has `PermissionRequest` hooks. Norn has no permission dialog system. Project/local shell hooks therefore remain rejected; implicit repository consent is not in scope.
- **Prompt-based hooks.** Claude Code supports `"type": "prompt"` for LLM-evaluated decisions. Expensive and complex. The config schema reserves the `type` field so it can be added later, but implementation is deferred.
- **Notification hooks.** Norn has no notification system. Deferred until one exists.
- **Setup hooks.** CLI-specific lifecycle (`--init`, `--maintenance`). If needed, handled in norn-cli, not libnorn.
- **Managed settings tier.** Claude Code has IT-managed settings. Not applicable to Norn's deployment model.
- **Hook deduplication.** Claude Code deduplicates identical commands. Nice optimisation but not essential. Can be added later without design changes.
- **Real-time hook reload.** Settings are captured at startup. No hot-reload.

## Structure

```
crates/norn/src/integration/hooks/
├── mod.rs              — pub mod + re-exports (NH-001)
├── traits.rs           — existing 5 hook traits + HookOutcome + HookRegistry,
│                         moved from hooks.rs, HookOutcome::Modify added (NH-001)
├── new_traits.rs       — 6 new hook traits: UserPromptHook, StopHook,
│                         SubagentHook, SessionLifecycleHook, CompactionHook,
│                         PostToolFailureHook (NH-002)
├── config.rs           — HookEventType enum, HookGroupConfig, HookCommandConfig,
│                         settings deserialization types (NH-003)
├── matchers.rs         — HookMatcher: compiled regex, match-all, matching logic (NH-003)
├── input.rs            — HookInput struct and per-event serialization (NH-004)
├── output.rs           — HookOutput struct, decision parsing, HookOutcome mapping (NH-004)
├── shell.rs            — ShellCommandHook: process spawn, stdin pipe, timeout,
│                         exit code handling, implements all hook traits (NH-005)
└── loader.rs           — load_hooks_from_settings: parse settings files, build
                          ShellCommandHook instances, register on HookRegistry (NH-006)
```

Wiring changes in existing files:

```
crates/norn/src/integration/mod.rs          — update hooks re-exports (NH-001)
crates/norn/src/loop/tool_dispatch.rs       — handle HookOutcome::Modify (NH-001)
crates/norn/src/error.rs                    — expand HookType enum (NH-002)
crates/norn/src/loop/runner.rs              — wire StopHook + UserPromptHook (NH-007)
crates/norn/src/loop/compaction.rs          — wire CompactionHook (NH-007)
crates/norn/src/tools/agent/spawn.rs        — wire SubagentHook (NH-007)
crates/norn/src/loop/helpers.rs             — non-reentrant guard on session event
                                              dispatch, fire-and-forget for shell (NH-005)

crates/norn-cli/src/runtime/builder.rs      — load settings, build hooks, register (NH-006)
crates/norn-cli/src/runtime/wiring.rs       — session lifecycle hooks at start/end (NH-007)
```

## Current Inventory

### Existing Hook Infrastructure

> Pre-implementation survey (design-time snapshot). `integration/hooks.rs`
> has since been promoted to the `integration/hooks/` module per D1, and
> line references have drifted; consult the code for current locations.

| Component | File | Status |
|:----------|:-----|:-------|
| PreToolHook trait | `integration/hooks.rs` | Complete — dispatched in `tool_dispatch.rs:98` |
| PostToolHook trait | `integration/hooks.rs` | Complete — dispatched in `tool_dispatch.rs:140` |
| PreLlmHook trait | `integration/hooks.rs` | Complete — dispatched in `runner.rs:301` |
| PostLlmHook trait | `integration/hooks.rs` | Complete — dispatched in `runner.rs:329` |
| SessionEventHook trait | `integration/hooks.rs` | Complete — dispatched in `helpers.rs:110` |
| HookRegistry | `integration/hooks.rs` | Complete — sequential dispatch, first-Block-wins |
| HookOutcome | `integration/hooks.rs` | Proceed / Block only, no Modify |
| Hook enum | `integration/hooks.rs` | 5 variants (PreTool, PostTool, PreLlm, PostLlm, SessionEvent) |
| HookType (error) | `error.rs` | PreLlm variant only |
| LoopContext.hooks | `loop/loop_context.rs:76` | `Option<HookRegistry>` |
| LlmCallSummary | `integration/hooks.rs` | stop_reason, usage, event_count, error |
| ToolEnvelope | `tool/envelope.rs` | tool_call_id, tool_name, model_args, metadata (`runtime_inputs` deleted by owner ruling 2026-07-03 — DECISIONS-2026-07 §4) |
| ToolOutput | `tool/traits.rs` | content, is_error, duration |

### Existing Shell Execution Patterns

| Component | File | Relevance |
|:----------|:-----|:----------|
| run_prompt_command | `loop/loop_context.rs:279` | Spawns `sh -c`, timeout, captures stdout — reusable pattern |
| VariableStore shell eval | `integration/variables.rs` | Similar spawn pattern with timeout |

### Settings Convention

| Location | Purpose | Status |
|:---------|:--------|:-------|
| `~/.norn/` | User-global norn home | Exists — sessions, profiles, history |
| `~/.norn/settings.json` | User-global settings | Does not exist yet |
| `.norn/` | Project-level norn directory | Convention exists for profiles |
| `.norn/settings.json` | Project settings | Does not exist yet |
| `.norn/settings.local.json` | Local project overrides | Does not exist yet |

## Constraints

- CO1: All new files go in `crates/norn/src/integration/hooks/`. No new crate.
- CO2: Existing hook trait tests must pass unchanged after module promotion.
- CO3: No `.unwrap()` or `.expect()` in library code.
- CO4: All files under 500 lines of code (excluding tests, comments, whitespace).
- CO5: Sequential dispatch with first-Block-wins preserved for all pre-hooks.
- CO6: Hook timeout is required per command entry. No hardcoded default value.
- CO7: Settings captured at startup. No hot-reload.
- CO8: Shell session event hooks are fire-and-forget. Trait session event hooks are synchronous.
- CO9: Non-reentrant guard prevents recursive session event hook dispatch.
- CO10: `HookOutcome::Modify` available only from `PreToolHook`. Other pre-hooks return `Proceed` or `Block` only.
- CO11: Config event names use snake_case matching The Count's schema convention.
