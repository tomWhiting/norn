---
type: design
cluster: norn-provider-ws
title: "Norn Provider: WebSocket Transport for the OpenAI Responses API"
---

# Norn Provider: WebSocket Transport for the OpenAI Responses API

> **Status: blocked historical design input. Do not implement or dispatch the
> briefs/checklist from this document.** The Responses remediation plan now owns
> the prerequisite authorities. This design assumes the current lossy SSE
> mapper can remain unchanged, assigns retry/fallback policy to the connection
> pool, contains operational values that have not been owner-approved, and adds
> cache prewarm before the cache experiment. It must be replaced after P3-P4
> establish canonical items and event reconciliation, P5 establishes
> turn/cancellation ownership, P6 establishes retry and attempt accounting, P7
> establishes immutable request capabilities, and P8 decides cache policy. The
> replacement must be reconciled against current official OpenAI documentation
> and current Codex source. Provider WebSocket work may not edit the same
> provider modules in parallel with P3/P4.

## Intention

When this work is done, Norn's OpenAI provider connects to the Responses API
over a persistent WebSocket instead of establishing a fresh HTTP connection
for every request. The connection persists across requests within a session,
eliminating the 1-2 second TLS handshake overhead that the current
`pool_max_idle_per_host(0)` configuration forces on every call. The
60-minute server-side connection limit is handled invisibly — sessions that
run for hours reconnect without any caller awareness. If WebSocket fails,
the provider silently falls back to the existing HTTP+SSE path with no
capability loss.

The transport layer is an internal implementation detail. Callers see the
same `Provider` trait, the same `ProviderStream`, the same `ProviderEvent`
values. The only observable differences are faster time-to-first-token (from
connection reuse) and better resilience on long sessions (from
proper lifecycle management instead of hoping idle HTTP connections survive).

## Problem

The connection timeout investigation (resolved 2026-05-27, 5 commits on
main) fixed three root causes: reqwest `TotalTimeoutBody` killing SSE
streams at 120s, native-tls OCSP blocking causing 600+s TLS hangs, and
stale HTTP connections from server-side close. The fixes work — connections
establish in ~30ms, headers arrive in 1-3s, and multi-minute streams
complete reliably.

But the fixes include a structural workaround: `pool_max_idle_per_host(0)`
disables connection pooling entirely, forcing a fresh TCP+TLS handshake on
every request. This is safe (no stale connections) but slow (~1-2s overhead
per request). For a workflow running 50+ agent turns, that is 50-100s of
pure transport overhead.

The OpenAI Responses API is designed for WebSocket. The Codex reference
client connects via `wss://` with persistent connections and connection
reuse. HTTP+SSE is the fallback path, not the primary. The server
aggressively closes idle HTTP connections because it expects clients to
use WebSocket for session-scoped work.

Three specific problems with the current HTTP-only approach:

- **Per-request TLS overhead.** Every request pays ~1-2s for TCP+TLS.
  WebSocket pays this once per session.

- **No prewarm capability.** The Responses API supports `generate: false`
  requests that prime the server's prompt cache without generating output.
  This requires a persistent connection to be meaningful — the cache is
  connection-scoped. HTTP has no persistent connection.

- **No protocol acknowledgment.** The WebSocket protocol includes a
  `response.processed` message that the client sends after consuming a
  response. This tells the server the client is done with the response
  and enables connection-scoped state management. HTTP has no back-channel.

## Solution

### D1: WebSocket transport lives inside `openai/` as `websocket.rs`

The WS transport is the same OpenAI Responses API — same endpoint path
(`/responses`), same JSON payload structure, same event types, same error
classification. The only difference is wire framing: HTTP sends a POST and
reads SSE text lines; WS sends a JSON message and reads JSON messages.

Both transports share `build_payload()`, `map_sse_event()`, `sse_types`,
`tools.rs`, and `rate_limiter`. All of this code lives in `openai/`.
Placing the WS transport alongside as `openai/websocket.rs` (a transport
peer to `openai/sse.rs`) keeps shared code as sibling imports with no
cross-module dependencies.

Rejected alternative: separate `provider/websocket/` folder as a peer to
`openai/`. Forces every shared import (`map_sse_event`, `build_payload`,
`SseEvent`, `ResponsesApiPayload`, `classify_failed_error`) to cross
module boundaries. Creates a misleading structure where two folders
implement the same API with duplicated integration points.

### D2: Extract HTTP transport from `mod.rs` into `http.rs`

The `SenderProvider` struct and its `execute()` method (~220 lines of HTTP
request/retry/SSE-streaming logic) move to `openai/http.rs`. This creates
structural symmetry for the dual-transport architecture: `http.rs` handles
HTTP+SSE framing, `websocket.rs` handles WS framing, `mod.rs` coordinates
transport selection. Each transport is a self-contained module with clear
boundaries.

Note: `openai/mod.rs` is 1042 total lines but only ~384 lines of
production code (lines 392+ are `#[cfg(test)]` modules). The file is
already compliant with the 500-line rule. This extraction is motivated by
clean module separation, not a file-length violation.

After extraction, `mod.rs` retains: `OpenAiProvider` struct, `Provider`
impl with transport dispatch, `build_http_client()`, `WsSlot`/`WsState`,
and construction logic.

### D3: WS event mapping reuses `map_sse_event()`

WebSocket JSON messages arrive as `{ "type": "response.output_text.delta",
"delta": "..." }`. The existing `SseEvent` struct is
`{ event_type: String, data: serde_json::Value }`. The mapping is trivial:
extract the `type` field as `event_type`, use the whole JSON object as
`data`, pass through `map_sse_event()`. Zero event-mapping duplication
between transports.

This works because `map_sse_event()` reads `event.data.get("delta")`,
`event.data.get("item")`, etc. — the same JSON fields exist in both SSE
`data:` payloads and WS message bodies.

Rejected alternative: separate WS event mapper. The event types and data
shapes are identical across both transports. Duplication is unjustifiable.

### D4: Transport selection via internal discriminant

`OpenAiProvider` gains an internal `Transport` enum (`WebSocket` | `Http`).
WebSocket is the default for known OpenAI endpoints (`api.openai.com` and
`chatgpt.com`). HTTP is used for custom `base_url` endpoints where WS
support is unknown.

The `Provider::stream()` implementation dispatches to the WS or HTTP path
based on the current transport state. If the WS path fails after retries,
the transport switches to HTTP for the remainder of the session (sticky
fallback). This matches the Codex reference client's behavior.

Rejected alternative: separate `WsProvider` struct implementing `Provider`.
Duplicates all shared state (auth, config, rate limiter) and breaks the
single-provider-per-config model that callers depend on.

### D5: Connection pool with demand-driven scaling

WS connections are managed through a `WsPool` protected by
`tokio::sync::Mutex`. The pool holds multiple `WsSlot` entries, each
with its own connection and state:

```
WsPool {
    slots: Vec<WsSlot>,
    config: WsPoolConfig,
    cooldown_until: Option<Instant>,
    consecutive_all_failures: u32,
    current_backoff: Duration,
}

WsPoolConfig {
    min_connections: 4,       — static floor
    headroom: 2,              — spare connections above active count
    max_connections: 16,      — ceiling
    idle_timeout: 10 min,     — cleanup threshold for excess connections
    base_backoff: 5s,         — initial cooldown after all-fail
    max_backoff: 5 min,       — cap + circuit breaker threshold
    reconnect_margin: 5 min,  — reconnect at 55 min of 60-min limit
}

WsSlot { state: WsState, established_at: Option<Instant>, last_used: Instant }

enum WsState {
    Available(WsConnection),  — connection idle, ready for use
    InUse,                    — a caller has claimed this slot
    Disconnected,             — no connection; next caller should try establishing
    Disabled,                 — pool-level: WS permanently off for this session
}
```

**Pool sizing — dynamic warm floor with headroom:**

The pool target size is `max(min_connections, active_count + headroom)`,
capped at `max_connections`. This ensures spare connections are always
ready ahead of demand:

- 0 active → 4 available (static floor of `min_connections`)
- 4 active → 2 available (headroom kicks in: 4 + 2 = 6 total)
- 14 active → 2 available (14 + 2 = 16, at `max_connections`)
- 16 active → 0 available (at max, fall back to HTTP)

When active connections increase, the pool proactively establishes new
connections to maintain headroom — before a caller needs one. When
active connections decrease, idle cleanup brings the pool back down.

- `min_connections` (static floor, default 4): minimum pool size. Kept
  alive across idle periods regardless of headroom calculation.
- `headroom` (default 2): spare connections maintained above active count.
- `max_connections` (ceiling, default 16): pool will not grow beyond this.
- Scale down: connections above the target that have been idle longer than
  `idle_timeout` (default 10 min) are closed.

**Take-use-return pattern (per slot):**

The Mutex lock is held only for the brief `take`/`replace` operations
(microseconds), never for the streaming duration:

1. Lock pool. Scan slots for first `Available(conn)`: swap to `InUse`,
   take conn, unlock. If none available and pool below max: create new
   slot as `InUse`, unlock, establish fresh connection. If none available
   and pool at max: unlock, fall back to HTTP.
2. Use the connection for the full request-response cycle (send, stream
   events, `response.processed`).
3. Lock pool. If connection is healthy: swap slot to `Available(conn)`.
   If broken: swap to `Disconnected`. Unlock.

The `InUse` state serves as a guard against double-establishment on the
same slot: concurrent callers see `InUse` and move to the next slot or
establish a new one.

**Cooldown with escalation to Disabled:**

When establishment fails on a slot, that slot returns to `Disconnected`.
If ALL recent establishment attempts across all slots fail, the pool
enters cooldown — callers use HTTP for a backoff period (starting at
`base_backoff`, default 5s). After the cooldown expires, the pool tries
WS again. Backoff doubles on each consecutive all-fail cycle
(5s → 10s → 20s → 40s → 80s → 160s → 300s). Successful establishment
resets the backoff to `base_backoff`.

**Circuit breaker:** when backoff reaches `max_backoff` (default 5 min)
and the final attempt after that cooldown also fails, the pool
transitions to `Disabled` — permanently HTTP for the remainder of the
session. This handles environments where WS will never work (corporate
proxies, network policies). `Disabled` is pool-level (WsState on the
pool, not per-slot) and irreversible within the session.

No caller is permanently stuck on HTTP unless WS establishment fails
through the full cooldown escalation to Disabled.

**Concurrent fork/sub-agent support:**

Forked children and spawned sub-agents share `Arc<OpenAiProvider>` and
call `stream()` concurrently. Each concurrent caller gets its own slot
from the pool. With `min_connections` of 4 and `headroom` of 2, a parent
+ 3 children all get WS simultaneously, and 2 spare connections are
already established for additional children. Callers beyond `max_connections`
fall back to HTTP for that request and rotate back to WS when a slot
frees up.

**Connection age management:**

Each slot tracks establishment time. Before using a connection, if age
exceeds `reconnect_margin` before the 60-minute limit (default: reconnect
at 55 minutes), the connection is proactively closed and a fresh one
established in its slot. If the server sends
`websocket_connection_limit_reached` during a request, the transport
marks the connection broken, establishes fresh, and retries once.

### D6: Auth adapter for WS handshake

`AuthProvider` gains a new async method:
`async fn auth_headers(&self) -> Result<Vec<(String, String)>, ProviderError>`.
This extracts the bearer token and `chatgpt-account-id` as header
name-value pairs, suitable for the WS upgrade handshake `HeaderMap`.
It is async because `OAuthAuthProvider` needs `self.manager.auth().await`
to retrieve the current token — same as `apply_auth`.

All three implementations (`OAuthAuthProvider`, `ApiKeyAuthProvider`,
`MockAuthProvider`) implement it. `MockAuthProvider::auth_headers` consumes
from the same token sequence as `apply_auth`, so tests can verify both
paths use the same credential source. The underlying credential retrieval
(AuthManager for OAuth, SecretString for API key) is shared — only the
output format changes from `reqwest::RequestBuilder` mutation to header
pairs.

Rejected alternative: reaching into `AuthManager` directly from
`websocket.rs`. Breaks the auth abstraction boundary.

### D7: `response.processed` acknowledgment

After the WS transport receives a `response.completed` event (mapped to
`ProviderEvent::Done`), it sends `{"type": "response.processed",
"response_id": "..."}` back on the WebSocket. This is part of the WS
protocol contract — the server uses it for connection-scoped state
management.

Failure to send `response.processed` (e.g., connection dropped between
receive and send) is logged at warn level but does not fail the request.
The response was already received; the acknowledgment is best-effort.

### D8: Prewarm as internal WS transport optimization

Prewarm is an internal behavior of the WS transport, not a method on the
`Provider` trait. The `Provider` trait stays clean with a single `stream()`
method. Callers do not know or control prewarm — it is a transport-level
optimization.

The WS transport may send a `generate: false` request internally to prime
the server's prompt cache before a real request. The decision of when to
prewarm is made by the transport based on connection state (e.g., first
request after establishing a fresh connection). The server processes the
context (tokenization, cache population) without generating output;
subsequent requests with the same context prefix benefit from cached
processing.

Prewarm is best-effort. In concurrent scenarios (forked children sharing a
provider), a prewarm may be wasted if another caller takes the connection
between the prewarm and the real request. No harm — the real request still
works, it just doesn't benefit from the cache prime. The common case
(single agent, sequential turns) always benefits.

Rejected alternative: `prewarm()` as a `Provider` trait method. Two of
three implementations (HTTP, Mock) would no-op. A transport optimization
should not leak into the provider abstraction. The trait stays single-method
and transport-agnostic.

### D9: No `previous_response_id`

Norn manages conversation state by including the full message history in
every request. `previous_response_id` is a server-side prompt-cache
optimisation that creates a dependency on server state — if the server
loses or evicts the response, the next request fails with an opaque error.
Our approach is stateless and resilient.

This is a deliberate non-goal, not an oversight.

### D10: `tokio-tungstenite` with rustls

The WebSocket connection uses `tokio-tungstenite` with rustls TLS.
`tokio-tungstenite` is already in the workspace — the `openai-oss-forks`
patch in `Cargo.toml` (lines 283-284) supports the `codex-login`
transitive dependency. Adding it as a direct dependency for `norn` requires
no new external crates.

TLS is via rustls (same backend as the rest of the workspace after the
native-tls removal). `permessage-deflate` compression is enabled to reduce
bandwidth on large conversation payloads.

## Goals

G1. WebSocket transport connects, sends requests, receives streaming events,
and produces identical `ProviderEvent` streams as the HTTP transport for the
same input.

G2. Connection reuse eliminates per-request TLS overhead. Second and
subsequent requests in a session reuse the existing WS connection.

G3. The 60-minute connection limit is handled seamlessly — sessions running
for hours reconnect without caller awareness or error propagation.

G4. HTTP fallback activates automatically if WS transport fails, with no
caller-visible error beyond `tracing::warn` log lines.

G5. `response.processed` is sent after every completed response on the WS
transport.

G6. Prewarm is available as an internal WS transport optimization for
priming the server's prompt cache on fresh connections.

G7. HTTP transport logic lives in `http.rs`, WS transport logic lives in
`websocket.rs`, `mod.rs` coordinates transport selection. Each transport
is a self-contained module.

## Non-Goals

NG1. **`previous_response_id`.** We manage state manually — full history
in every request. Server-side state dependency is an explicit non-goal (D9).

NG2. **Binary or audio WebSocket frames.** Text-only JSON protocol at this
time. Voice mode transport is a separate cluster (VM-A/B/C).

NG3. **Multiple concurrent requests on a single WS connection.** The
Responses API processes one request at a time per connection. Concurrent
callers each get their own connection from the pool (D5).

NG4. **Connection sharing across provider instances.** Each `OpenAiProvider`
owns its own `WsPool`. Sharing pools across provider instances is a
separate concern if ever needed.

NG5. **Custom WebSocket endpoints.** WS is only attempted for known OpenAI
endpoints. Custom `base_url` endpoints use HTTP (D4).

## Structure

```
crates/norn/src/provider/
├── mod.rs                  — re-exports [unchanged]
├── traits.rs               — Provider trait [unchanged]
├── auth.rs                 — AuthProvider [+async auth_headers method, D6]
├── events.rs               — ProviderEvent [unchanged]
├── request.rs              — ProviderRequest [unchanged]
├── usage.rs                — Usage [unchanged]
├── debug.rs                — DebugDumper [+ws_send, ws_lifecycle entry types]
├── agent_event.rs          — AgentEventSender [unchanged]
├── mock.rs                 — MockProvider [unchanged]
│
└── openai/
    ├── mod.rs              — OpenAiProvider, WsPool, transport dispatch, build_http_client [MODIFIED, D4/D5]
    ├── http.rs             — HTTP+SSE transport, SenderProvider::execute [NEW, D2]
    ├── websocket.rs        — WS transport, connection lifecycle, framing [NEW, D1/D5]
    ├── request.rs          — build_payload [unchanged]
    ├── sse.rs              — SseParser, map_sse_event [unchanged, reused by WS via D3]
    ├── sse_types.rs        — wire deserialization structs [unchanged]
    ├── tools.rs            — serialize_tool [unchanged]
    └── rate_limiter.rs     — RateLimiter [unchanged]
```

Files introduced by this cluster:
- `openai/http.rs` — HTTP transport extracted from mod.rs
- `openai/websocket.rs` — WebSocket transport, connection manager, framing

Files modified by this cluster:
- `openai/mod.rs` — WsPool/WsSlot/WsState, transport dispatch logic
- `provider/auth.rs` — async `auth_headers` method on AuthProvider trait + MockAuthProvider
- `provider/debug.rs` — `ws_send` and `ws_lifecycle` DebugPayload variants
- `crates/norn/Cargo.toml` — `tokio-tungstenite` direct dependency

## Current Inventory

### Provider Files (current state)

| File | Lines | Status |
|------|-------|--------|
| `openai/mod.rs` | 1042 (384 prod) | Compliant. ~384 production lines, ~658 lines of tests. Contains HTTP transport (~220 lines) to be extracted for structural symmetry. |
| `openai/sse.rs` | 1251 | Contains SseParser + map_sse_event. map_sse_event is transport-agnostic, reusable by WS. |
| `openai/request.rs` | 669 | build_payload() + ResponsesApiPayload. Transport-agnostic, reusable by WS. |
| `openai/sse_types.rs` | 240 | Wire deserialization types. Transport-agnostic. |
| `openai/tools.rs` | 146 | serialize_tool(). Transport-agnostic. |
| `openai/rate_limiter.rs` | 164 | Token-bucket rate limiter. Transport-agnostic. |
| `provider/auth.rs` | 659 | AuthProvider trait. Currently HTTP-only (takes reqwest::RequestBuilder). |
| `provider/debug.rs` | 325 | DebugDumper. HTTP-specific entry types only. Needs WS variants. |
| `provider/traits.rs` | 60 | Provider trait. Single method, transport-agnostic. Unchanged by this cluster. |
| `provider/mock.rs` | 179 | MockProvider. Unchanged by this cluster. |

### Key Functions Reused by WS Transport

| Function | File | What WS uses it for |
|----------|------|---------------------|
| `build_payload()` | `openai/request.rs:69` | Same JSON body, wrapped in WS envelope |
| `map_sse_event()` | `openai/sse.rs:207` | Maps WS JSON events to ProviderEvent |
| `classify_failed_error()` | `openai/sse_types.rs` | Same error classification for WS errors |
| `extract_usage()` | `openai/sse.rs:422` | Usage extraction from response.completed |
| `serialize_tool()` | `openai/tools.rs:12` | Tool definitions in request payload |

### Workspace Dependencies

| Crate | Current Status | Change Needed |
|-------|----------------|---------------|
| `tokio-tungstenite` | Transitive dep via codex-login, patched to openai-oss-forks | Add as direct dep for norn |
| `rustls` | Workspace dep with aws_lc_rs | Already configured, used by WS TLS |
| `reqwest` | Direct dep, rustls features | Retained for HTTP fallback |
| `http` | Transitive | May need direct dep for HeaderMap in auth_headers |

## Constraints

CO1. No `.unwrap()` or `.expect()` in library code.

CO2. All files under 500 lines of production code (excluding tests,
comments, whitespace).

CO3. WS transport uses rustls, never native-tls. The OCSP blocking fix
(commit `04901476`) removed native-tls from the workspace for this reason.

CO4. HTTP fallback path is the exact existing code extracted to `http.rs`,
not a reimplementation. Behavioral parity is verified by existing tests.

CO5. Auth secrets handled through the `AuthProvider` trait, never accessed
directly by transport code.

CO6. Connection lifecycle events (establish, reconnect, close, fallback)
logged at debug level via `tracing`. Failures logged at warn level.

CO7. WS connection established lazily on first request, not eagerly at
provider construction. Matches the `pool_max_idle_per_host(0)` behavior
where no idle connections exist.

CO8. `response.processed` failure does not fail the request. The response
is already consumed; acknowledgment is best-effort.

CO9. Don't re-enable HTTP connection pooling. The `pool_max_idle_per_host(0)`
setting in `build_http_client()` stays. HTTP is the fallback path and the
server still closes idle HTTP connections aggressively.

CO10. Don't use reqwest `timeout()` for SSE streams. If per-request
deadlines are ever needed on the HTTP path, use `connect_timeout()` and
`read_timeout()` only.
