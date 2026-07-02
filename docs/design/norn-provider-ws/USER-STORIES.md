# Norn-Provider-Ws — User Stories

## AI Agent — Making Provider Requests

**S1.** As an AI agent, I want my provider requests to use a persistent WebSocket connection from a pool so that I don't pay 1-2 seconds of TLS handshake overhead on every request.

**S2.** As an AI agent, I want the WebSocket connection to reconnect seamlessly when the 60-minute server limit is reached so that long sessions complete without transport errors.

**S3.** As an AI agent, I want provider requests to fall back to HTTP automatically if WebSocket is unavailable so that no request is lost due to transport issues.

**S13.** As an AI agent running concurrently with siblings, I want my own WebSocket connection from the pool so that I don't block or serialize against other agents.

**S14.** As an AI agent, I want the provider to recover WebSocket connectivity after transient failures so that I'm not stuck on HTTP for the rest of the session.

## AI Agent — Optimizing Latency

**S4.** As an AI agent, I want the WebSocket transport to automatically prime the server's prompt cache on fresh connections so that time-to-first-token is reduced without any action on my part.

**S5.** As an AI agent, I want the provider to acknowledge completed responses on the WebSocket so that the server can manage connection state correctly.

## Workflow Engine — Running Agent Steps

**S6.** As the workflow engine, I want the provider to handle transport selection automatically so that workflow steps don't need transport-level configuration.

**S7.** As the workflow engine, I want long-running workflow sessions to maintain provider connectivity across many agent turns so that multi-hour workflows complete reliably.

## Human Developer — Maintaining the Provider

**S8.** As a developer, I want HTTP and WebSocket transports to share the same event mapping code so that provider behavior is consistent and changes are made in one place.

**S9.** As a developer, I want the HTTP transport extracted to its own file so that HTTP and WS transports are structurally symmetric self-contained modules.

**S10.** As a developer, I want WS transport debug output in the same JSONL format as HTTP so that debug-api dumps are transport-agnostic.

## Human Operator — Troubleshooting Provider Connectivity

**S11.** As an operator, I want transport fallback events logged with reasons so that I can diagnose why the provider switched from WebSocket to HTTP.

**S12.** As an operator, I want connection lifecycle events logged at debug level so that I can trace WebSocket establishment, reconnection, and closure.
