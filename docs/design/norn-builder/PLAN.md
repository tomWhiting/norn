# Norn Builder API — Implementation Plan

**Owner:** Pythagoras  
**Goal:** Replace ClaudeProcess subprocess dispatch with in-process Norn agent execution via AgentBuilder.  
**Constraint:** No v1 scoping — every runtime capability from day one.

## Overview

Workflow steps currently spawn Claude CLI subprocesses. The builder replaces this with in-process execution, enabling prompt caching, streaming integration, and full Norn lifecycle (hooks, rules, diagnostics).

## P1: working_dir on ToolContext

Add `working_dir: PathBuf` field + `resolve_path()` helper to ToolContext. Update every filesystem tool to resolve relative paths against it. Update LoopContext with working_dir for prompt commands, hooks, rules. All 12 filesystem tools + 6 non-tool Command sites updated.

## P2: CancellationToken in runner

Add tokio-util dep. Add AgentStepResult::Cancelled. Thread Option<CancellationToken> through run_agent_step. Check at loop top + tokio::select! on provider call.

## C1: AgentBuilder struct

New file: crates/norn/src/agent/builder.rs. Fluent builder with all runtime capabilities. ToolPreset enum for common tool sets. AgentOutput return type with stop_reason enum. Error model: retryable errors handled internally, terminal errors propagated, tool/schema errors are model feedback.

## Implementation Order

P1 and P2 parallel. Then C1 (builder). Then A1 (Marge's adapter).

## Ownership

Pythagoras: P1, P2, C1. Marge: A1 (adapter, event bridge, cancel bridge).
