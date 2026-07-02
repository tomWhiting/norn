---
type: design
cluster: norn
title: "Norn: Headless Agent Runtime"
---

# Norn: Headless Agent Runtime

## Intention

Norn exists so that AI agents can be orchestrated like infrastructure, not operated like desktop applications. When this is done, launching an agent is a function call. Its output is typed and schema-validated. Its tools validate their own results. Its context is surgically controlled. Its operations are auditable at the level healthcare and legal regulators require.

The experience should feel like programming against a well-designed library: predictable inputs, guaranteed output shapes, composable pieces, and no hidden state. The runtime does not make decisions about what to do. It executes what the orchestrator tells it to, with full transparency about what happened and why.

## Problem

Every agent harness on the market is a standalone interactive terminal application. They are designed for a human sitting at a keyboard, running one agent at a time, reading its output, and deciding what to do next.

This does not work when you need to run 14 agents concurrently, each with different tools and roles, producing structured output that feeds into the next stage of a workflow, while maintaining an audit trail that can explain every decision.

The Meridian/Yggdrasil infrastructure already provides orchestration (Rhai scripting, workflow engine), profile management (model, tools, reasoning effort, instructions per role), diagnostic reporting (compiler-grade), syntax analysis (tree-sitter, LSP), and Claude integration (Claude Runner). What is missing is the runtime underneath: the thing that takes an instruction and a tool set, calls an LLM, executes tool calls, validates output, and returns typed results. That runtime does not exist as a standalone library. Every implementation bundles it with a TUI, a CLI, session management, authentication, and provider-specific logic that cannot be separated.

Norn is that runtime, extracted to its essence.

## Solution

### D1: Library crate, not an application

Norn is a Rust library crate (`libnorn` or similar). No binary, no TUI, no CLI. The orchestrator, Rhai scripts, and workflow engine call into it. It follows the Meridian pattern: standalone crate first, integrated into Meridian second. It cannot depend on Meridian-specific types at the crate level.

### D2: Two providers, not a compatibility layer

Claude is accessed through Claude Runner and Claude Code (legitimate subscription path). OpenAI is accessed directly via the Responses API. Norn does not pursue provider count. Each provider is implemented with full awareness of its capabilities and subscription economics. Additional providers can be added later; the provider trait is the extension point.

### D3: Schema-validated structured output as the contract

Every agent step declares an output schema. The runtime validates output before returning. If validation fails, the error and schema are fed back to the model for retry. For OpenAI, `response_format` provides native enforcement. Structured output is the contract between the agent and the orchestrator, not a feature flag.

Schema enforcement is implemented as a dynamic tool: a structured-output tool whose schema is set at invocation time, which the model calls as its final action. This is provider-agnostic (works with any model that supports tool calling) and follows the same pattern Claude Code uses for `--json-schema`. Per-event schemas (D4) can use the same mechanism: every message becomes a tool call against the appropriate schema.

Schema validation has a retry budget (default 2-3 attempts). If the model cannot produce valid output after exhausting retries, Norn returns a typed result variant: `SchemaUnreachable { best_attempt, validation_errors, attempts }`. The orchestrator decides how to handle it (relax schema, provide more context, switch model, accept partial output). The runtime does not silently loop until token thresholds kill it.

In practice, schema failures are rare when the schema is well-formed. When they occur, the cause is almost always something else going wrong (context issues, wrong model, instruction problems), not model incapability.

### D4: Per-event output schemas

Schemas apply to multiple event types, not just the final response. Assistant messages, spoken responses (for TTS/accessibility), tool call envelopes, stop output, questions, handoffs, reviews, and progress updates can each have their own schema. Profiles configure which schemas are active.

### D5: Tool lifecycle with embedded validation

Tools have four phases: pre-validate, execute, post-validate, on-success. Each phase has compile-time (baked into the tool) and runtime (profile/policy-configured) components. The Write tool blocks if the file exists and has not been read. The Edit tool validates the AST after editing. Diagnostics run after edits. Formatters run on success. This is not hooks bolted on after the fact. It is the tool knowing its own job.

Compile-time checks are non-negotiable by default but can respect explicit orchestrator context flags for legitimate overrides. Example: a workflow generating files from templates can set `allow_overwrite: true` on the Write tool context, skipping the read-before-overwrite check. The check code is always present and always runs; it reads a flag that the orchestrator must actively set. The default (no flag) is the safe behavior. This is not profile config — it is an active choice by workflow code that knows the intent.

### D6: Tool call envelopes with open metadata

Tool calls carry a runtime envelope: model-supplied arguments, runtime-supplied inputs (inbound messages, diagnostics, filesystem changes accumulated since the last tool boundary), and an open metadata field the model can populate with tags, task references, or whatever the consumer needs. The framework does not enforce metadata content; it makes it available for downstream consumption.

### D7: Rules engine separate from hooks

Rules are contextual guidance that fires based on conditions (path globs, bash command matches, tool invocations). They fire before or after matched actions. Three delivery modes: append to system context, inject at the next input boundary, or deliver as a message. Rules provide information. Hooks intercept and modify. They are complementary systems.

Rule lifecycle uses dependency inversion: the runtime emits generic path-change and tool-invocation events (not rule-specific events). The rules engine consumes these events alongside anything else that cares about path state. Prompt construction emits content-inclusion events as it assembles each prompt. The rules engine tracks which rules are currently present by consuming these events passively, and re-injects rules when they have been removed from context by compaction or aging. Prompt construction is authoritative and unaware of rules. The rules engine is a passive consumer.

### D8: No agent concurrency limits

The agent registry tracks active agents, hierarchical paths, roles, and statuses. It does not cap agent count. Resource constraints are handled at the provider and infrastructure level. Any hardcoded limit will be wrong.

### D9: Forking as a first-class primitive

An agent can fork itself onto a different model for bounded tasks (commits, message triage, log analysis, research). The fork inherits filtered context, executes, and returns a structured audit result to the parent. This is a tool, not an orchestrator feature.

### D10: AI-monitored background tasks

Long-running commands and sub-agents are monitored by a lightweight model. The parent queries the monitor for structured updates instead of reading raw output. This avoids the fundamental sub-agent problem: the parent either blocks or consumes all the context it was trying to save.

### D11: Immutable session events, surgical context construction

Session events are append-only. Context editing marks events as suppressed, summarized, or superseded but never deletes them. Prompt construction is a view over the event stream: the system chooses what to include at each turn. The audit trail is always complete.

### D12: Extension system via Meridian extension protocol

Extensions are external processes communicating via the Meridian extension protocol (manifest-based registration, HTTP routes, shared runtimes). SDKs exist for Rust, Python, and TypeScript. Extensions are shared across agents. Norn does not embed any scripting language runtime for extensions. Rhai handles in-process workflow scripting; extensions handle everything else.

### D13: Norn as MCP server

Norn can expose all its tools as an MCP server. Other agents and harnesses (including Claude Code) connect to Norn's MCP server to use its enhanced tools without Norn being the primary harness.

### D14: Norn-wrapped Claude Code

A third integration mode: Claude Code launches stripped to bare metal (no native tools, replaced system prompt). Norn provides its tools via MCP. Norn captures events into its session format. Claude Code retains its session history for legitimate resumption. This gives Claude models access to Norn's tools while staying within Anthropic's terms.

## Goals

G1. An agent step can be invoked as a single function call that returns typed, schema-validated output.

G2. Write and Edit tools catch syntax errors via tree-sitter before committing changes to disk.

G3. The rules engine fires contextual guidance based on path globs and command matches without spamming repeated injections.

G4. Multiple agents run concurrently with no hardcoded concurrency limits, sharing a common registry.

G5. Forking an agent onto a cheaper model for a bounded task is a single tool call that returns a structured audit result.

G6. The runtime streams token-by-token events for real-time observability.

G7. Norn runs as a standalone MCP server exposing its tool set to any consumer.

G8. Claude Code can be wrapped by Norn (tools via MCP, events captured) without violating Anthropic's terms.

G9. The session event model is append-only with surgical context construction (suppress, summarize, inject, compact) that never destroys the audit trail.

## Non-Goals

NG1. TUI or CLI. Norn is a library. Visual interfaces are built on top by consumers (Meridian web view, future TUI).

NG2. Provider count. Two providers (Claude via Runner, OpenAI direct) are the initial scope. More can be added; chasing provider count is not a goal.

NG3. Graph-backed session intelligence in v1. Memgraph integration, hybrid memory search (Tezera, ColBERT), and workflow reflection are designed for but not implemented in the initial pass.

NG4. Copy-on-write filesystem layers. This is a Yggdrasil feature, not a Norn feature. Captured in VISION.md for future work.

NG5. Pi extension ecosystem compatibility. Possible via a bridge extension later. Not initial scope.

NG6. Embedded scripting language runtime for extensions. No QuickJS, no Lua, no Deno embedded in the binary.

## Structure

```
crates/norn/
  src/
    lib.rs                    -- public API surface
    error.rs                  -- crate error types (thiserror)
    provider/
      mod.rs                  -- Provider trait, ProviderEvent enum
      openai.rs               -- OpenAI Responses API implementation
    loop/
      mod.rs                  -- agent loop entry point
      context.rs              -- context construction and surgical editing
      schema.rs               -- output schema validation and retry
      events.rs               -- streaming event types
      iteration.rs            -- threshold detection, soft handoff, continuation
    tool/
      mod.rs                  -- Tool trait, ToolRegistry, ToolEnvelope
      lifecycle.rs            -- pre-validate, post-validate, on-success phases
      scheduling.rs           -- effect-based parallel execution
      risk.rs                 -- bash risk classification
      availability.rs         -- dynamic tool gating by profile/stage/context
    tools/
      mod.rs                  -- core tool registration
      read.rs                 -- Read tool
      write.rs                -- Write tool (AST validation, file length, read-before-overwrite)
      edit.rs                 -- Edit tool (AST validation, blast radius, read-before-edit)
      patch.rs                -- ApplyPatch tool (tree-sitter aware)
      search.rs               -- unified Search tool (ripgrep, nucleo, ast-grep, vector, keyword)
      bash.rs                 -- Bash tool (risk classification, streaming, progress)
      web.rs                  -- WebSearch and WebFetch tools
      lsp.rs                  -- LSP tool (hover, definition, references, diagnostics)
      agent.rs                -- SpawnAgent, SendMessage, WaitAgent, CloseAgent, Fork tools
      task.rs                 -- Task management tool
      skill.rs                -- Skill activation tool
      script.rs               -- RunScript (inline Rhai execution)
      tool_search.rs          -- ToolSearch (BM25/semantic tool discovery)
    rules/
      mod.rs                  -- rules engine entry point
      parser.rs               -- YAML front matter + trigger parsing
      triggers.rs             -- path glob and command match evaluation
      lifecycle.rs            -- in-context tracking and re-injection logic
      delivery.rs             -- system context, context injection, message delivery modes
    agent/
      mod.rs                  -- agent registry, agent state
      registry.rs             -- AgentRegistry (paths, statuses, spawn reservation)
      mailbox.rs              -- inter-agent messaging (sender, receiver, sequence numbers)
      fork.rs                 -- forking logic (context filtering, model switching, audit)
      monitor.rs              -- AI-monitored background tasks (RunMonitored)
      goals.rs                -- goal tracking (objectives, budgets, continuation)
    session/
      mod.rs                  -- session event model
      events.rs               -- event types (message, model change, compaction, fork, label, custom)
      store.rs                -- append-only event storage
      context_edit.rs         -- suppress, summarize, inject, compact operations
    integration/
      mod.rs                  -- integration entry points
      claude.rs               -- Claude Runner integration (StepOutcome mapping)
      mcp_client.rs           -- MCP client (connect to external tool servers)
      mcp_server.rs           -- MCP server (expose Norn tools)
      rhai.rs                 -- Rhai builtins (run_agent, spawn_agent, etc.)
      hooks.rs                -- lifecycle hooks (pre/post tool, pre/post LLM, session events)
      variables.rs            -- session variable system (declarative, scriptable)
      diagnostics.rs          -- diagnostics crate integration
  Cargo.toml
```

## Constraints

CO1. Pure Rust. No C dependencies. `unsafe_code = "deny"`.

CO2. Tokio as the async runtime. All async code uses tokio primitives.

CO3. No hardcoded limits on agent count, file sizes, or iteration counts unless the design explicitly specifies one (and it specifies very few).

CO4. No `unwrap()` or `expect()` in library code. Mutex poison handled explicitly. All error paths propagated via `thiserror`.

CO5. No file over 500 lines of code (excluding tests, comments, whitespace).

CO6. `mod.rs` contains only `pub mod` declarations and re-exports. Logic goes in named files.

CO7. Session events are append-only. Context editing operations mark events; they never delete them.

CO8. Structured output schemas are enforced, not advisory. If the model does not conform, the runtime retries with the validation error.

CO9. Tool pre-validation can block execution. Write blocks if the file has not been read. This is not configurable.

CO10. Norn does not depend on Meridian-specific types at the crate level. Integration with Meridian happens through trait implementations and configuration.
