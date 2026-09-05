---
type: design
cluster: norn-channel-settings
title: Persisted channel policy and optional default wake
---

# Persisted channel policy and optional default wake

> **Cluster:** norn-channel-settings

## Intention

Reduce repeated channel flags without weakening source identity or MCP approval.

## Problem

Channel policy and retention currently exist only as mandatory CLI arguments.

## Solution

One typed settings section and merged resolver, with optional capability negotiation for a default policy.

## Decisions

- Root settings channels is a typed partial object: default_policy off|wake|next-turn; sources map of named off|wake|next-turn; max_retained_messages and max_retained_bytes positive integers; overflow reject-new. Hold remains library-only until inbox controls exist.
- Persist in existing user/project/local settings.json and settings.local.json locations. Fieldwise precedence user < project < local < -c channels JSON < dedicated channel flags. Source maps merge by name; explicit off overrides a lower named delivery. Empty maps do not clear inherited entries. Missing or null optional fields do not override. No channel section means disabled; absent default policy means off.
- New section refuses unknown fields, duplicate fields, duplicate source names, unknown policy values and zero limits. A second -c channels object is rejected by name. Inline errors and Debug never echo raw JSON.
- No invented retention quotas or overflow: any enabled channel selection requires both explicit limits and reject-new from merged settings or flags. Inactive configurations may retain limits for later use; no channel runtime is built when all selected policies are off.
- Default delivery is optional negotiation only for enabled, approved stdio MCP sources declaring claude/channel. Ordinary MCP servers keep working. Named delivery is required and refuses missing capability, unknown name, disabled server or HTTP source. Named off may exclude disabled or HTTP definitions but still refuses unknown names.
- Existing project MCP approval remains the process authority boundary; persisted channel settings cannot approve a server, expand tool access or bypass generation fencing. Project/local channel policy applies only after that authority boundary.
- Install the notification listener before initialization. Optional missing capability retires staged input but preserves the tool client; malformed capability fails explicitly. No polling, extra provider request or history scan is introduced.
- Effective policy is validated for the actual mode after settings merge and before provider/MCP construction. Next-turn is interactive-only, including TUI fallback checks. Driven retains one active run.
- Channel policy and limits are immutable for this launch. MCP reload applies that same policy to refreshed definitions; changing policy files requires restart.
- Use existing --mcp-config strict mcpServers documents unchanged. -c channels JSON expresses the channel object, not a second general settings loader.
- Optional default selection preserves existing per-server connection-failure isolation: an unavailable or malformed default-selected source is reported visibly and excluded while healthy sources publish. Explicit named Required delivery still fails startup/candidate on connection or capability failure. A malformed declaration always fails its source connection; it never becomes a tools-only successful connection.

## Structure

| Path | Note | Brief |
|------|------|-------|
| `crates/norn/src/config/mod.rs` | NCS-001 R1 exact file wall |  |
| `crates/norn/src/config/types.rs` | NCS-001 R1 exact file wall |  |
| `crates/norn/src/config/merge/mod.rs` | NCS-001 R1 exact file wall |  |
| `crates/norn/src/config/merge/settings.rs` | NCS-001 R1 exact file wall |  |
| `crates/norn/src/config/channels.rs` | NCS-001 R1 exact file wall |  |
| `crates/norn/src/config/channels_tests.rs` | NCS-001 R1 exact file wall |  |
| `crates/norn/src/config/merge/channels.rs` | NCS-001 R1 exact file wall |  |
| `crates/norn-cli/src/config/assembly.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn-cli/src/config/mod.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn-cli/src/cli/channel_args.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn-cli/src/runtime/channel_config.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn-cli/src/runtime/channel_config_tests.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn-cli/src/runtime/resolve.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn-cli/src/tui/driver.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn-cli/src/print/orchestrator.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn-cli/src/config/channel_overrides.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn-cli/src/config/channel_overrides_tests.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn/src/integration/mcp_channel_settings.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn/src/integration/mcp_channels.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn/src/integration/mcp_channel_source.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn/src/integration/mcp_channel_inbox.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn/src/integration/mcp_client.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn/src/integration/mcp_candidate_builder.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn/src/integration/mcp_runtime_candidate.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn/src/integration/mod.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn/src/agent/mcp_channel_builder_tests.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn/tests/mcp_channels_stdio.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn/tests/support/mcp_channels_fixture.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn/src/integration/mcp_channel_selection_tests.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn-cli/tests/mcp_launch.rs` | NCS-001 R4 exact file wall |  |
| `crates/norn-cli/tests/support/mcp_launch_fixture.rs` | NCS-001 R4 exact file wall |  |
| `docs/MCP-LAUNCH.md` | NCS-001 R5 exact file wall |  |
| `docs/release-notes/UNRELEASED.md` | NCS-001 R5 exact file wall |  |
| `docs/design/norn-cli/DRIVEN-PROTOCOL.md` | NCS-001 R5 exact file wall |  |
| `docs/design/norn-mcp-launch/design.json` | NCS-001 R5 exact file wall |  |
| `docs/design/norn-mcp-launch/DESIGN.md` | NCS-001 R5 exact file wall |  |
| `docs/design/norn-mcp-launch/briefs/NML-001.json` | NCS-001 R5 exact file wall |  |
| `docs/design/norn-mcp-launch/briefs/NML-001.md` | NCS-001 R5 exact file wall |  |
| `crates/norn-cli/src/runtime/channel_startup.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn/src/integration/mcp_runtime.rs` | NCS-001 R3 exact file wall |  |
| `crates/norn-cli/src/print/mod.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn-cli/src/print/driven.rs` | NCS-001 R2 exact file wall |  |
| `crates/norn-cli/src/print/assembly.rs` | NCS-001 R2 exact file wall |  |
