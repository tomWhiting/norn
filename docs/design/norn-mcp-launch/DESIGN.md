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

The original --extension URI flag could not express args, env or HTTP headers. NML-001 resolves complete startup definitions for all modes. The completed NML-001/NV-002 checkpoint is exact commit `5227db49d805a4c0729912dca3832c842c8f39e1`, installed at 20:15:11 Melbourne on 5 September 2026 and merged to local/remote main. Installed SHA-256: `dc61b730918db4928e5e9ff0cf1194f7ed0fdfcf0d010286ab87f8d2d26e0a19`. Its exact 205 battery passed all six declared legs with 14 venue bindings; native build and installed help/list/redacted-error checks passed. Proof: `/private/tmp/nwp-04-proof/exact-battery-2/execution-result.json` and `/private/tmp/nwp-04-proof/native-release-2/installation.json`. Fresh Fable re-review remains pending; no new review pass or waiver is claimed. The historical `3964799` battery remains red: five green legs and tests exit 101. Its detailed cancellation error was not retained. NV-002 changed only two test cleanup waits; the green repaired-candidate receipt does not identify the old failure cause or relabel the old result. The earlier 18:09 `450bb7a` defaults installation and the `911eddd` receipt remain historical.

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
