# P0 live MCP candidate — 2026-07-14

**Status:** Candidate ready for independent review. This record does not grant
Gate D acceptance or close whole-phase P0.

**Range:** `6e6d8db..edd936a`

## Outcome

This slice turns the retained startup MCP configuration into a live,
programmatic control surface shared by embedded callers, print mode, and the
TUI. Configuration mutations are serialized through one bounded actor. A
candidate connects and discovers every eligible server before one immutable
tool generation and its exact runtime pool are published together. A failed
candidate or failed publication leaves the prior pair active.

The root view may select a subset without discarding the complete connected
pool. Spawned and forked agents derive their own server view from that pool,
inherit explicit selection, and refresh their provider definitions at each
request boundary. An in-flight request retains one generation lease through
tool dispatch, so publication cannot split advertised definitions from the
executor that implements them.

## User surfaces

- `AgentHandle::mcp_control()` exposes list, inspect, session mutation,
  persistent mutation, approval, revocation, and reload operations.
- CLI and TUI `/mcp` share one parser and redacted renderer for `help`, `list`,
  `inspect`, `add`, `remove`, `enable`, `disable`, `approve`, `revoke`, and
  `reload`.
- Session `add` accepts stdio environment values and HTTP header values without
  rendering them. TUI definition commands, including malformed `/mcp add`
  attempts, bypass persistent prompt history.
- Enabling a disabled inherited definition preserves its source provenance.
  An enabled project definition has a distinct fingerprint and remains pending
  until that exact operational form is approved.
- If the approval ledger cannot open, direct user/local/CLI/session control
  remains live. Project definitions stay pending and approval operations return
  a typed error.

## Protocol and lifecycle

- Stdio uses a concurrent inbound pump for responses, server requests, and
  notifications. Streamable HTTP handles the same message classes on active
  SSE response streams.
- `roots/list`, `ping`, `notifications/roots/list_changed`, pagination, and
  `notifications/tools/list_changed` are implemented with JSON-RPC envelope
  validation.
- Contextual root replacement plus `tools/call` is serialized per shared
  client, preventing root and child calls from overwriting each other's
  context. A failed root notification restores the prior local view.
- Tool-list notifications rediscover on the same connection and publish a new
  runtime/generation pair. A notification observed before watcher subscription
  is not lost; a second notification during rediscovery schedules the latest
  revision after the first publication.
- Watchers own only a notification receiver and a weak controller sender.
  Removed or replaced clients abort their watcher; dropping the last request
  lease releases the transport and its descriptor permit.

## Verification

- `cargo check -p norn -p norn-cli -p norn-tui --all-targets`: pass.
- `cargo clippy -p norn -p norn-cli -p norn-tui --all-targets -- -D warnings`:
  pass, with no suppression.
- `cargo test -p norn --lib --quiet`: pass at the final agent-runtime assembly
  shape.
- The touched-crate all-target battery exposed one stale TUI catalog
  expectation; after correcting the contract, `cargo test -p norn -p norn-cli
  -p norn-tui --all-targets --quiet` passed. Its TUI portion included 678 unit
  tests and 17 PTY tests.
- `2026-07-14-mcp-live-policy.json` covers 90 changed Rust files at `edd936a`:
  zero production files over 500 lines, zero thin-entrypoint violations, and
  zero added matches for unwrap, expect, panic, todo, unimplemented, lint
  allowances, ignored tests, or debt markers. The largest changed production
  files are 499, 497, 449, and 444 lines.
- `run_mcp_live_evidence.sh 20` records 20/20 passes for each of seven cases:
  pre-subscription notification, change-during-refresh, watcher release,
  contextual-call serialization, child lease rollover, stdio descriptor
  release, and TUI history non-persistence.

## Explicit residuals

- Streamable HTTP does not yet maintain an idle standalone GET listener.
  Therefore server notifications are processed when received on an active SSE
  response stream; this slice does not claim arbitrary idle HTTP push.
- Reconnect, session resumption, HTTP session DELETE, MCP OAuth, sampling,
  resources, and prompts remain outside this slice.
- Inline CLI values can be present in the invoking shell's process arguments;
  this slice claims Norn log/history/rendering non-disclosure, not shell-history
  control.
- A live definition mutation may wait for connection and discovery up to the
  existing MCP request deadline. Background connection UX is not claimed.

## Independent review questions

1. Can any publication path expose a generation paired with a different
   runtime, especially across root and child request boundaries?
2. Can a stale or duplicated watcher retain a removed stdio client, republish a
   replaced definition, or lose a notification that arrives during refresh?
3. Can root and child contextual calls interleave between root notification and
   tool dispatch on one shared client?
4. Can any CLI/TUI parse, error, inspection, history, or debug path disclose an
   environment value, header value, URL credential, query, fragment, or private
   failure body?
5. Does enabling an inherited project definition ever reuse approval for its
   disabled fingerprint or change its project-controlled provenance?
