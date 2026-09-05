---
type: design
cluster: norn-mcp-launch
title: Runtime MCP launch documents
---

# Runtime MCP launch documents

> **Cluster:** norn-mcp-launch

## Intention

Accept complete named MCP definitions at process launch for both interactive and driven Norn, without persistent configuration writes.

## Problem

The existing --extension URI flag cannot express args, env or HTTP headers; driven callers need the same complete startup configuration. Verification is under repair: the exact 3964799 battery returned five green legs and tests rc101. NV-002 documents the two test-only cleanup-notification changes. The named current cancellation failure has no retained detailed error, so its cause is not attributed to the historical candidate6 timeout. Proof: /private/tmp/nwp-04-proof/exact-battery/execution-result.json. New candidate verification and installation remain pending.

## Solution

Strict repeatable --mcp-config JSON|PATH documents feed the existing CLI configuration overlay and MCP lifecycle.

## Decisions

- Ordinary Claude channel push is implemented; permission relay and persistent daemon are outside this unit.
- Use --mcp-config JSON|PATH, repeatable, mcpServers object, existing settings types and CLI overlay. CLI definitions replace lower-layer definitions as complete named entries.
- Reject same-launch duplicate server names and duplicate JSON object keys; never silently choose a duplicate.
- No default channel caps: retain explicit --channel policy/count/bytes/overflow configuration.
- No RPC request schema change: driven uses launch flags and retains its single-run lifecycle.
- Interpret type as transport through the existing settings schema; command/args/env are process data, not shell text.
- Reject invalid input without logging inline documents or secret values.

## Structure

| Path | Note | Brief |
|------|------|-------|
| `crates/norn-cli/src/cli/args.rs` | NML-001 R1 exact file wall |  |
| `crates/norn-cli/src/config/mod.rs` | NML-001 R1 exact file wall |  |
| `crates/norn/src/config/types.rs` | NML-001 R1 exact file wall |  |
| `crates/norn-cli/src/config/mcp_launch.rs` | NML-001 R1 exact file wall |  |
| `crates/norn-cli/src/config/mcp_launch_tests.rs` | NML-001 R1 exact file wall |  |
| `crates/norn-cli/src/runtime/resolve.rs` | NML-001 R2 exact file wall |  |
| `crates/norn-cli/src/commands/mcp_config.rs` | NML-001 R2 exact file wall |  |
| `crates/norn-cli/tests/mcp_launch.rs` | NML-001 R2 exact file wall |  |
| `crates/norn-cli/tests/support/mcp_launch_fixture.rs` | NML-001 R2 exact file wall |  |
| `docs/release-notes/UNRELEASED.md` | NML-001 R3 exact file wall |  |
| `docs/design/norn-cli/DRIVEN-PROTOCOL.md` | NML-001 R3 exact file wall |  |
| `docs/design/norn-channels/briefs/NCH-001.json` | NML-001 R3 exact file wall |  |
| `docs/design/norn-channels/briefs/NCH-001.md` | NML-001 R3 exact file wall |  |
| `docs/design/norn-channels/briefs/NCH-002.json` | NML-001 R3 exact file wall |  |
| `docs/design/norn-channels/briefs/NCH-002.md` | NML-001 R3 exact file wall |  |
| `docs/design/norn-channels/briefs/NCH-003.json` | NML-001 R3 exact file wall |  |
| `docs/design/norn-channels/briefs/NCH-003.md` | NML-001 R3 exact file wall |  |
| `docs/design/norn-channels/briefs/NCH-004.json` | NML-001 R3 exact file wall |  |
| `docs/design/norn-channels/briefs/NCH-004.md` | NML-001 R3 exact file wall |  |
| `docs/design/norn-channels/briefs/NCH-005.json` | NML-001 R3 exact file wall |  |
| `docs/design/norn-channels/briefs/NCH-005.md` | NML-001 R3 exact file wall |  |
| `docs/MCP-LAUNCH.md` | NML-001 R3 exact file wall |  |
| `crates/norn-cli/Cargo.toml` | NML-001 R2 exact file wall: register protocol-clean custom test harness only |  |
| `crates/norn-cli/src/tui/driver.rs` | NML-001 R2 amended file wall: preserve typed startup argument and authentication exit codes |  |
