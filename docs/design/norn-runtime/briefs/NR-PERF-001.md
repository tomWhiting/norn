---
type: brief
id: NR-PERF-001
cluster: norn-runtime
title: Workflow executor performance — connection pool, profile cache, runtime contention
---

# NR-PERF-001: Workflow executor performance — connection pool, profile cache, runtime contention

> **Cluster:** norn-runtime

## Purpose

Eliminate the 3x performance gap between the norn headless CLI and the in-process workflow executor path by fixing connection pooling, caching resolved profiles, removing synchronous DB writes from the critical path, and sharing diagnostic infrastructure across steps.

## Task

Fix the four highest-impact performance bottlenecks in the workflow executor's Norn step runner. Each fix is isolated — they can be applied independently. Verify with cargo check, clippy, and tests on meridian-services and norn crates after each change.

## Requirements

### R1: Enable HTTP connection pool reuse

Remove or change pool_max_idle_per_host(0) at crates/norn/src/provider/openai/mod.rs:127. The current setting forces a new TCP+TLS handshake for every API call. Set it to a reasonable value (e.g. 4) or remove the line entirely to use reqwest's default connection pooling. Verify that the existing http2_keep_alive_interval and http2_keep_alive_timeout settings on lines 128-130 are still applied.

**Acceptance:**
- pool_max_idle_per_host is NOT set to 0
- HTTP/2 keepalive settings are preserved
- Existing provider tests pass
- No new unsafe code

**Files:**
- modify: crates/norn/src/provider/openai/mod.rs

### R2: Cache resolved profile across step setup

In make_norn_step_runner (crates/meridian-services/src/workflow/imperative_callbacks.rs), the profile is resolved via norn::profile::resolve_profile three times per step: line 488 for prompt metadata, line 623 via profile_tool_names for Meridian tool gating, and again inside AgentBuilder::build at crates/norn/src/agent/builder.rs:405. Resolve the profile ONCE at the start of the step callback, then pass the resolved Profile object to all three consumers. AgentBuilder needs a method that accepts a resolved Profile instead of a profile name string — add .profile(resolved: Profile) alongside the existing .profile_name(name).

**Acceptance:**
- Profile is resolved exactly once per step (not three times)
- AgentBuilder accepts a pre-resolved Profile via a new .profile() method
- profile_tool_names uses the already-resolved profile instead of re-resolving
- Prompt metadata uses the already-resolved profile instead of re-resolving
- Existing tests pass — agent behavior is identical

**Files:**
- modify: crates/meridian-services/src/workflow/imperative_callbacks.rs
- modify: crates/norn/src/agent/builder.rs

### R3: Make step metadata DB writes non-blocking

In make_norn_step_runner (imperative_callbacks.rs), the calls to storage.add_step (line 466) and storage.complete_step (line 688) use runtime_handle.block_on() which blocks the thread synchronously. Move add_step to fire-and-forget via the existing event_sink channel or a spawned task — the step_row_id it returns is only used for optional metadata (prompt payload), not for agent execution. Move complete_step to a spawned async task that runs after the step result is returned to the Rhai script.

**Acceptance:**
- add_step no longer blocks the step thread before agent execution begins
- complete_step no longer blocks the step thread before returning to Rhai
- Step metadata is still persisted (rows still appear in the database)
- Prompt metadata (expanded_command) is still persisted when step_row_id is available
- Existing tests pass

**Files:**
- modify: crates/meridian-services/src/workflow/imperative_callbacks.rs

### R4: Share diagnostic infrastructure across steps

In make_norn_step_runner (imperative_callbacks.rs:580), build_diagnostic_infra(&wd) is called every step, rebuilding adapter registries, policy registries, and re-parsing CONVENTIONS.toml. Move the construction outside the per-step callback — build it once when make_norn_step_runner is called, wrap in Arc, and clone into each step invocation.

**Acceptance:**
- build_diagnostic_infra is called once per execution, not once per step
- The diagnostic infra is shared via Arc across all steps in the same execution
- Diagnostic behavior is identical — same adapters, same policies, same CONVENTIONS.toml
- Existing tests pass

**Files:**
- modify: crates/meridian-services/src/workflow/imperative_callbacks.rs

## Boundaries

- Do NOT change any prompts, schemas, or profile content
- Do NOT change the Norn agent loop or tool dispatch logic
- Do NOT change the Rhai evaluator or workflow script format
- Do NOT change the OpenAI API request/response format
- Do NOT remove session persistence (JSONL) — it must still write session files
- Do NOT remove step metadata persistence (add_step/complete_step) — just make it non-blocking

## Verification

- cargo check -p norn -p meridian-services --all-targets
- cargo clippy -p norn -p meridian-services -- -D warnings
- cargo test -p norn -p meridian-services
- Verify pool_max_idle_per_host is not 0 in the built reqwest client
- Verify profile resolution happens exactly once per step via tracing output or debug logging
