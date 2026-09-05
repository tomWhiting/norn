---
type: design
cluster: norn-channels
title: Channels-compatible ingress for an owned running Norn session
---

# Channels-compatible ingress for an owned running Norn session

> **Cluster:** norn-channels

## Intention

Claude Code Channels wire compatibility is high value and the next feature priority alongside review remediation. A stdio MCP server may push an external message into a running Norn session, wake it when permitted, or leave input held for an inbox. The feature must work with Rust runtimes and must not depend on the new daemon, Aion or full-screen TUI.

## Problem

The current MCP reader consumes Channels notifications and discards initialization instructions. Existing inter-agent buffers do not provide total retained-byte accounting or an external-source ownership contract.

## Solution

The existing running session remains the single owner. Transport ingress is a capability passed explicitly by that owner; sharing tools across children does not share message reception.

McpChannelInbox is one recipient-scoped retained queue. Host-supplied positive count and byte budgets cover staged, held, queued and claimed messages. Receipt/state mutations are synchronous short critical sections; delivery waits use Notify/watch, not polling.

The connection object is installed in ClientProtocolState before spawning the stdio reader. Messages can stage before asynchronous initialize completion; they cannot reach a provider before the capability is verified and host activation publishes that generation.

A replacement connection is pending until host activation. Activation fences new events from the former connection. Already admitted events retain their original source identity; candidate-only events are explicitly rejected if the candidate is abandoned.

The RPC reader never awaits inbox capacity. It records a typed refusal when admission is unavailable and continues reading tool responses. No unbounded spill queue or implicit retry task is added.

The consumer claims, processes/persists, then explicitly settles a retained message. Moving or inspecting it never frees quota. Cancellation returns the claim to the retained queue.

Source, connection generation, recipient, sequence and local event identity are host-stamped. Sender metadata is stored separately. A supplied meta.source remains untrusted data and cannot replace the configured source attribute.

Hold waits for explicit release/deny. NextTurn joins independently started work. Wake makes idle work eligible through a push notification; busy work uses a safe provider boundary. These are explicit policy choices, not inferred from urgent metadata.

R1-R3 source implements the receiver/admission seam and Rust stdio fixture, with venue verification pending. They do not provide a complete Channels launch feature. R4-R6 remain outlines for runtime ownership, current controls, delivery persistence and actual running-session interoperability.

NCH-003 R4 first CLI release: interactive TUI accepts Wake and NextTurn; plain print (including pipes and terminal fallback) and driven JSON-RPC accept Wake only during the active run. Explicit jsonrpc is always one-shot. Hold is refused until CLI release/deny controls exist; that operator-control work remains open, while the library retains Hold with explicit host release/deny. Unsupported CLI combinations fail before provider/MCP startup with exit 2, a named stderr diagnostic and empty stdout. No daemon, durable inbox, automatic linger or second driven run is introduced.

## Principles

- **P1** — Source-enable rule and scope: Explicit named session/source opt-in, separate from enabling MCP tools. Must be recorded before CLI wiring; first slice requires a caller-created attachment.
- **P2** — Delivery policy: library callers retain explicit Hold, NextTurn and Wake; the first CLI release accepts only the actual-mode combinations in NCH-003 R4. No policy is selected or coerced implicitly.
- **P3** — Total retained count and bytes: Required positive host-supplied values; existing 32-message coordination setting is not a full lifecycle bound and no byte total is currently ruled. No new default. Test budgets are fixture inputs only.
- **P4** — Overflow: Explicit RejectNew policy with visible source/generation/refusal state while RPC continues. R1-R3 accept explicit enum choice; operator default not imposed.
- **P5** — Restart and replay: Transport staging is explicitly in-memory. R4 must define persistent accepted events and uncertain-append recovery before advertising durable inbox admission. Upstream IDs remain metadata until a separate dedupe contract is chosen. No exactly-once or upstream receipt claim.

## Non-Goals

- No new daemon, PTY/multiplexer, TUI workspace, permission relay, hidden gameplay evaluation, private Claude implementation or automatic approval authority.
- No feature-complete claim from a receiver fixture alone.
- No numeric product default or artificial queue limit chosen by an implementer.

## Structure

| Path | Note | Brief |
|------|------|-------|
| `crates/norn/src/integration/mcp_channel_frame.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/src/integration/mcp_channel_inbox.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/src/integration/mcp_channel_inbox_tests.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/src/integration/mcp_channel_source.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/src/integration/mcp_channel_tests.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/src/integration/mcp_channels.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/src/integration/mcp_client.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/src/integration/mcp_protocol.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/src/integration/mcp_stdio.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/src/integration/mcp_wire.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/src/integration/mod.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/tests/mcp_channels_stdio.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/tests/support/mcp_channels_fixture.rs` | NCH-001 exact R1-R3 wall; purpose supplied by numbered row | NCH-001 |
| `crates/norn/Cargo.toml` | R3 custom Rust integration harness prevents libtest stdout from contaminating MCP frames | NCH-001 |
| `crates/norn/src/loop/mod.rs` | NCH-001 R4a exact owned delivery wall | NCH-001 |
| `crates/norn/src/loop/loop_context.rs` | NCH-001 R4a exact owned delivery wall | NCH-001 |
| `crates/norn/src/loop/runner/setup.rs` | NCH-001 R4a exact owned delivery wall | NCH-001 |
| `crates/norn/src/loop/runner/dispatch.rs` | NCH-001 R4a exact owned delivery wall | NCH-001 |
| `crates/norn/src/loop/runner/tests.rs` | NCH-001 R4a exact owned delivery wall | NCH-001 |
| `crates/norn/src/loop/linger.rs` | NCH-001 R4a exact owned delivery wall | NCH-001 |
| `crates/norn/src/loop/mcp_channel_delivery.rs` | NCH-001 R4a exact owned delivery wall | NCH-001 |
| `crates/norn/src/loop/mcp_channel_delivery_tests.rs` | NCH-001 R4a exact owned delivery wall | NCH-001 |
| `crates/norn/src/loop/runner/tests/mcp_channel_delivery.rs` | NCH-001 R4a exact owned delivery wall | NCH-001 |
| `crates/norn/src/tool/selection.rs` | NCH-002 preserves dynamic tool availability policy separately from startup visibility | NCH-002 |
| `crates/norn/src/tool/registry.rs` | NCH-002 preserves dynamic tool availability policy separately from startup visibility | NCH-002 |
| `crates/norn/src/tool/generation.rs` | NCH-002 preserves dynamic tool availability policy separately from startup visibility | NCH-002 |
| `crates/norn/src/tool/mod.rs` | NCH-002 preserves dynamic tool availability policy separately from startup visibility | NCH-002 |
| `crates/norn/src/tool/generation_tests.rs` | NCH-002 preserves dynamic tool availability policy separately from startup visibility | NCH-002 |
| `crates/norn/src/integration/mcp_runtime.rs` | NCH-002 preserves dynamic tool availability policy separately from startup visibility | NCH-002 |
| `crates/norn/src/integration/mcp_candidate_builder_tests.rs` | NCH-002 preserves dynamic tool availability policy separately from startup visibility | NCH-002 |
| `crates/norn-cli/src/cli/args.rs` | Explicit channel startup and current-session wake wiring | NCH-003 |
| `crates/norn-cli/src/cli/mod.rs` | Explicit channel startup and current-session wake wiring | NCH-003 |
| `crates/norn-cli/src/cli/channel_args.rs` | Explicit channel startup and current-session wake wiring | NCH-003 |
| `crates/norn-cli/src/runtime/mod.rs` | Explicit channel startup and current-session wake wiring | NCH-003 |
| `crates/norn-cli/src/runtime/resolve.rs` | Explicit channel startup and current-session wake wiring | NCH-003 |
| `crates/norn-cli/src/runtime/channel_config.rs` | Explicit channel startup and current-session wake wiring | NCH-003 |
| `crates/norn-cli/src/runtime/channel_config_tests.rs` | Explicit channel startup and current-session wake wiring | NCH-003 |
| `crates/norn-cli/src/runtime/channel_startup.rs` | Explicit channel startup and current-session wake wiring | NCH-003 |
| `crates/norn-cli/src/print/orchestrator.rs` | Explicit channel startup and current-session wake wiring | NCH-003 |
| `crates/norn-cli/src/tui/driver.rs` | Explicit channel startup and current-session wake wiring | NCH-003 |
| `crates/norn-tui/src/app/event_loop.rs` | Explicit channel startup and current-session wake wiring | NCH-004 |
| `crates/norn-tui/src/app/turn/run.rs` | Explicit channel startup and current-session wake wiring | NCH-004 |
| `crates/norn-tui/src/app/turn/mod.rs` | Explicit channel startup and current-session wake wiring | NCH-004 |
| `crates/norn-tui/src/error.rs` | Explicit channel startup and current-session wake wiring | NCH-004 |
| `crates/norn-tui/Cargo.toml` | Explicit channel startup and current-session wake wiring | NCH-004 |
| `crates/norn-tui/tests/mcp_channels.rs` | Explicit channel startup and current-session wake wiring | NCH-004 |
| `crates/norn-tui/tests/support/mcp_channels_tui.rs` | Explicit channel startup and current-session wake wiring | NCH-004 |
| `crates/norn/src/agent/mcp.rs` | Actual root ownership and coherent initial MCP channel activation | NCH-005 |
| `crates/norn/src/agent/builder/build.rs` | Actual root ownership and coherent initial MCP channel activation | NCH-005 |
| `crates/norn/src/integration/mcp_candidate_builder.rs` | Actual root ownership and coherent initial MCP channel activation | NCH-005 |
| `crates/norn/src/integration/mcp_runtime_candidate.rs` | Actual root ownership and coherent initial MCP channel activation | NCH-005 |
| `crates/norn/src/integration/mcp_control.rs` | Actual root ownership and coherent initial MCP channel activation | NCH-005 |
| `crates/norn/src/integration/mcp_control_actor.rs` | Actual root ownership and coherent initial MCP channel activation | NCH-005 |
| `crates/norn/src/integration/mcp_control_error.rs` | Actual root ownership and coherent initial MCP channel activation | NCH-005 |
| `crates/norn/src/integration/mcp_channel_settings.rs` | Actual root ownership and coherent initial MCP channel activation | NCH-005 |
| `crates/norn/src/integration/mcp_runtime_channels.rs` | Actual root ownership and coherent initial MCP channel activation | NCH-005 |
| `crates/norn/src/integration/mcp_channel_startup_tests.rs` | Actual root ownership and coherent initial MCP channel activation | NCH-005 |
| `crates/norn/src/agent/mcp_channel_builder_tests.rs` | Actual root ownership and coherent initial MCP channel activation | NCH-005 |
| `crates/norn/src/provider/agent_event.rs` | R4a distinct persisted external-channel delivery observation on existing bounded event broadcast | NCH-001 |
| `crates/norn/src/provider/mod.rs` | R4a distinct persisted external-channel delivery observation on existing bounded event broadcast | NCH-001 |
| `crates/norn/src/provider/channel_event.rs` | R4a distinct persisted external-channel delivery observation on existing bounded event broadcast | NCH-001 |
| `crates/norn-cli/src/print/output.rs` | Distinct typed external-channel observation consumed by the existing display path | NCH-003 |
| `crates/norn-cli/src/print/output/provider_events.rs` | Distinct typed external-channel observation consumed by the existing display path | NCH-003 |
| `crates/norn-tui/src/app/dispatch.rs` | Distinct typed external-channel observation consumed by the existing display path | NCH-004 |
| `crates/norn-tui/src/app/turn/mid.rs` | Distinct typed external-channel observation consumed by the existing display path | NCH-004 |
| `crates/norn/src/loop/runner/tests/streaming_nudge.rs` | R4a exhaustive provider-only regression accounts for distinct channel delivery observations | NCH-001 |
| `crates/norn/src/integration/mcp_control_approval.rs` | Approval/revocation generation rollback followed by the same owner-scoped runtime/channel publication as startup | NCH-005 |
| `crates/norn-cli/src/print/mod.rs` | NCH-003 R2: declare the private print input module for the production-LOC extraction | NCH-003 |
| `crates/norn-cli/src/print/input.rs` | NCH-003 R2: existing piped-stdin reading and prompt composition, with unchanged public orchestrator re-export | NCH-003 |
| `crates/norn-cli/tests/channel_mode_policy.rs` | NCH-003 R4 actual-binary unsupported mode/policy preflight, stdout purity and no-launch regressions | NCH-003 |
| `crates/norn/src/integration/mcp_control_refresh.rs` | NCH-005 refresh recovery never restores stale runtime after a committed channel publication error | NCH-005 |
| `crates/norn/src/integration/mcp_channel_publication_tests.rs` | NCH-005 deterministic regressions for revoked-source fencing, committed configuration coherence and exhaustive channel transitions | NCH-005 |

## Constraints

- **K1** — R1-R3 receiver passed 10 Rust stdio cases, 122 MCP tests and strict Clippy on 205 as reported by coordinator. R4a now has an exact executable wall; later runtime publication and operator wiring remain outlines.
- **K2** — Parent centralizes all Rust builds and required 205 venue verification; source-bound exact-commit battery and Fable rereview remain separate landing requirements.
- **K3** — Official public reference: https://code.claude.com/docs/en/channels-reference
- **K4** — McpChannelInbox::new(recipient_id, McpChannelLimits::new(count, bytes)?) -> McpChannelInbox; inbox.host() -> McpChannelHost
- **K5** — McpChannelHost::attachment(policy: McpChannelPolicy, overflow: McpChannelOverflow) -> McpChannelAttachment
- **K6** — McpClient::connect_with_channel(config, roots, attachment) -> Result<McpClient, IntegrationError>; config.name and newly minted instance_id bind source and generation before reader spawn.
- **K7** — McpClient::activate_channel() publishes verified staged input; McpClient::retire_channel() fences further ingress; lifecycle status remains inspectable.
- **K8** — McpChannelInbox::try_claim()/claim() -> McpChannelDelivery; message() borrows immutable host-attributed data; consume() settles; host.deny(id) settles unclaimed held input; Drop returns an unsettled claim.
- **K9** — McpChannelHost::status()/subscribe_status() exposes count/bytes and latest typed refusal; McpChannelInbox::wake_ready() observes readiness without draining held input.
- **K10** — /Users/tom/Developer/ablative/apps/cambium/surface/mcp/src/notifications.ts: urgent is boolean true; official meta requires strings. Existing source metadata must not replace host attribution. Separate adapter correction to urgent="true" and tests; preserve source metadata as untrusted data.
- **K11** — /Users/tom/Developer/games/hammerbarn-40k/src/mcp/channel.ts: String-only metadata with chat_id, message_id, revision and notification_kind is conforming. Use equivalent Rust fixture envelopes; no production game edits in R1-R3.
- **K12** — NCH-003 R4 first CLI release: interactive TUI accepts Wake and NextTurn; plain print (including pipes and terminal fallback) and driven JSON-RPC accept Wake only during the active run. Explicit jsonrpc is always one-shot. Hold is refused until CLI release/deny controls exist; that operator-control work remains open, while the library retains Hold with explicit host release/deny. Unsupported CLI combinations fail before provider/MCP startup with exit 2, a named stderr diagnostic and empty stdout. No daemon, durable inbox, automatic linger or second driven run is introduced.
