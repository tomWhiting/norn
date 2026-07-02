# Norn-Provider-Ws — Checklist

## HTTP Transport Extraction

- [ ] **C1** — SenderProvider struct and execute() method extracted from openai/mod.rs to openai/http.rs.
- [ ] **C2** — HTTP transport in http.rs is structurally symmetric with websocket.rs — each transport is a self-contained module.
- [ ] **C3** — HTTP transport produces identical ProviderEvent stream as before extraction (no behavioral change).

## WebSocket Connection

- [ ] **C4** — WebSocket connection established to wss://api.openai.com/v1/responses with rustls TLS.
- [ ] **C5** — WebSocket connection established to wss://chatgpt.com/backend-api/codex/responses for OAuth auth.
- [ ] **C6** — WS upgrade handshake includes OpenAI-Beta header with version from named constant WS_BETA_VERSION.
- [ ] **C7** — Auth bearer token and chatgpt-account-id applied during WS upgrade handshake via AuthProvider::auth_headers.
- [ ] **C8** — permessage-deflate compression enabled on WS connection.
- [ ] **C9** — Ping/pong frames handled automatically by tokio-tungstenite.

## WebSocket Request and Response

- [ ] **C10** — Request payload wrapped in {"type": "response.create", ...} envelope before WS send.
- [ ] **C11** — WS Text messages parsed as JSON with type field extracted as event_type and fed through map_sse_event().
- [ ] **C12** — ProviderEvent stream from WS transport matches HTTP transport for identical request inputs.
- [ ] **C13** — WS error envelopes ({"type": "error", ...}) mapped to ProviderError with classified error types via classify_failed_error().

## Connection Pool

- [ ] **C14** — WsPool struct holds Vec<WsSlot>, pool config (min/max connections, idle timeout), cooldown state, and failure counter.
- [ ] **C15** — WsSlot struct holds WsState, established_at timestamp, and last_used timestamp.
- [ ] **C16** — WsState enum defined with four variants: Available(WsConnection), InUse, Disconnected, Disabled.
- [ ] **C17** — Pool protected by tokio::sync::Mutex. Lock held only for slot scan and state swap (microseconds), never for streaming duration.
- [ ] **C18** — Dynamic warm floor: target pool size is max(min_connections, active_count + headroom), capped at max_connections.
- [ ] **C19** — Pool proactively establishes connections to maintain headroom before callers need them.
- [ ] **C20** — Pool shrinks on idle: connections above the dynamic target closed after idle_timeout of inactivity.
- [ ] **C21** — Caller arriving when all slots InUse and pool at max_connections falls back to HTTP for that request.
- [ ] **C22** — WsPoolConfig holds min_connections (4), headroom (2), max_connections (16), idle_timeout (10 min), base_backoff (5s), max_backoff (5 min), reconnect_margin (5 min) — all configurable via ProviderConfig.

## Per-Slot State Machine

- [ ] **C23** — Caller finding Available takes connection and swaps slot to InUse.
- [ ] **C24** — Caller finding Disconnected swaps slot to InUse and attempts WS establishment.
- [ ] **C25** — InUse state guards against double-establishment on same slot: concurrent callers move to next slot.
- [ ] **C26** — After request: healthy connection returned as Available, broken connection marked Disconnected.
- [ ] **C27** — Connection reused across sequential requests on the same slot without re-establishing TLS.

## Pool Recovery

- [ ] **C28** — When all recent establishment attempts fail, pool enters cooldown with exponential backoff.
- [ ] **C29** — During cooldown, all callers use HTTP. After cooldown expires, pool tries WS again on next request.
- [ ] **C30** — Successful establishment resets cooldown backoff to zero.
- [ ] **C31** — No caller is permanently stuck on HTTP unless WS establishment fails through full cooldown escalation to Disabled.
- [ ] **C32** — Callers that used HTTP due to pool-full rotate back to WS when a slot frees up on subsequent requests.
- [ ] **C33** — Circuit breaker: when backoff reaches max_backoff and final attempt fails, pool transitions to Disabled (irreversible within session).

## Connection Lifecycle

- [ ] **C34** — Per-slot connection age tracked from establishment time via std::time::Instant.
- [ ] **C35** — Proactive reconnection triggered when connection age exceeds reconnect_margin before the 60-minute limit.
- [ ] **C36** — websocket_connection_limit_reached error triggers fresh connection and single request retry.
- [ ] **C37** — Connection dropped and recreated on any unexpected WS Close frame from server.
- [ ] **C38** — Warm floor established via parallel tokio::spawn on first pool access. First caller waits only for its own connection, remaining establish in background.

## Protocol Acknowledgment

- [ ] **C39** — response.processed message sent on WS after receiving ProviderEvent::Done with response_id.
- [ ] **C40** — response.processed send failure logged at warn level and does not fail the request.

## Prewarm

- [ ] **C41** — WS transport supports internal generate: false prewarm on fresh connections as a transport-level optimization.
- [ ] **C42** — Prewarm is best-effort — wasted without harm if another caller uses the connection before the real request.
- [ ] **C43** — No prewarm method on the Provider trait. Provider trait remains single-method (stream only).

## Transport Selection and Fallback

- [ ] **C44** — WS transport attempted first for api.openai.com and chatgpt.com endpoints.
- [ ] **C45** — HTTP transport used for custom base_url endpoints without WS attempt.
- [ ] **C46** — HTTP fallback always available — pool-full, cooldown, Disabled, and establishment failure all degrade to HTTP.
- [ ] **C47** — Transport fallback logged at warn level with failure reason.

## Auth Adapter

- [ ] **C48** — AuthProvider trait gains async auth_headers method returning Vec<(String, String)> of header name-value pairs.
- [ ] **C49** — OAuthAuthProvider implements auth_headers via AuthManager::auth().await, returning bearer token and chatgpt-account-id.
- [ ] **C50** — ApiKeyAuthProvider implements auth_headers returning bearer token only.
- [ ] **C51** — MockAuthProvider implements auth_headers consuming from the same token sequence as apply_auth.

## Debug and Observability

- [ ] **C52** — DebugDumper gains write_ws_send method producing ws_send JSONL entry type for outgoing WS frames.
- [ ] **C53** — DebugDumper gains write_ws_lifecycle method producing ws_lifecycle JSONL entry type for connection events.
- [ ] **C54** — Incoming WS events logged via existing write_sse_event after SseEvent construction (same entry type as HTTP).
- [ ] **C55** — WS transport logs connection establishment, request start, event count, and completion at debug level.
- [ ] **C56** — Pool state changes (scale up, scale down, cooldown enter/exit, Disabled) logged at debug level.

## Dependencies

- [ ] **C57** — tokio-tungstenite added as direct dependency in norn Cargo.toml with rustls-tls features.
- [ ] **C58** — No new external crates introduced beyond tokio-tungstenite (already in workspace patch).
