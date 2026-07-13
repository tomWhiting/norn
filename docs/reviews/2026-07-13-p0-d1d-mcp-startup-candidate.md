# P0 D1D MCP startup candidate

Date: 2026-07-13  
Status: implementation candidate; independent review pending

## Scope

This slice turns the dormant `mcp_servers` settings map into a normal agent
startup input without claiming the later live-mutation surface. Effective
server definitions retain their winning source across
`user < project < local < CLI < session`, replace wholesale by name, and may be
masked with `enabled: false`.

Shared-project definitions are visible but inactive until their canonical
project plus normalized definition fingerprint is approved. User, private
local, CLI, and session definitions are direct operator scopes and do not
receive a second approval prompt. `norn mcp list`, `approve`, and `revoke` use
the existing `-C` project context.

## Product boundary

MCP is not a filesystem sandbox. A configured server may operate across the
computer according to its own configuration and operating-system access. MCP
roots are dynamic workspace context, not confinement. The sole extra activation
step is for a server definition sourced from checked-in project configuration;
user-owned, private project-local, CLI, and session definitions are direct
operator input.

## Runtime behavior

- Stdio and Streamable HTTP definitions convert into redacted typed client
  configs; stdio receives an explicit agent working directory.
- Connected clients live in one `McpRuntime` attached to `AgentBuilder` and the
  tool context, including zero-tool servers. Connections run independently, so
  one broken optional server does not prevent healthy servers or Norn itself
  from starting.
- Discovered tools are exposed as `mcp__<server>__<leaf>` while `tools/call`
  sends the original leaf name. Provider-facing names are bounded, safe, and
  pair-qualified; registration rejects existing and intra-MCP collisions.
- The client negotiates MCP `2025-11-25`, requires the initialize result's
  protocol/capability/server-info fields, advertises initialized, answers server
  ping requests, validates JSON-RPC shape and response IDs, and follows
  `tools/list` pagination with repeated-cursor rejection.
- HTTP POSTs advertise JSON and SSE, retain the negotiated session identifier,
  send the negotiated protocol header on subsequent requests, and handle JSON
  or SSE response bodies. Stdio cancellation after a write invalidates the
  channel rather than letting a stale response satisfy the next request.
- Root settings select all or named startup servers. Variants and spawned agents
  inherit or select another subset from the connected pool without launching a
  duplicate process. An empty selection removes MCP tools while retaining other
  tools.
- A corrupt approval ledger leaves shared-project servers pending and does not
  prevent unrelated direct-scope servers from connecting.

## Evidence

- `cargo clippy -p norn -p norn-cli --all-targets -- -D warnings`: pass.
- `cargo fmt --all --check` and `git diff --check`: pass.
- `cargo test -p norn --lib -q`: 3,197 passed, zero failed.
- `cargo test -p norn-cli --lib -q`: 450 passed, zero failed.
- `cargo test -p norn mcp --lib`: 36 passed, zero failed.
- The immediately preceding full `norn` run observed one unrelated existing
  process-output timing failure after 3,196 passes:
  `model_output_is_incremental_and_unknown_id_is_none` saw an empty first poll.
  Its isolated rerun distribution was 20/20 pass, followed by the recorded
  3,197/3,197 full pass above. This is disclosed as observed nondeterminism, not
  presented as a stability proof for that separate process fixture.
- [`2026-07-13-p0-d1d-mcp-policy.json`](evidence/2026-07-13-p0-d1d-mcp-policy.json)
  records the committed `5015e79..a949af1` range: zero added unwrap, expect,
  panic, todo, unimplemented, lint-suppression, ignore, or unresolved-marker
  matches; zero changed production files over 500 LOC; and zero thin-entrypoint
  violations.
- `runtime::mcp::tests`: three passed. The project fixture configures a shell
  marker command and proves the marker is not created while approval is
  pending. A workspace `.norn/settings.local.json` fixture proves it remains a
  shared project source. The user-owned project-local fixture completes a real
  stdio initialize/initialized/ping/tools-list exchange without approval,
  retains the healthy client beside a failed optional client, and exercises an
  empty per-agent view.

Production LOC inventory for every new module, counted to the first
`#[cfg(test)]` using the same `awk` method:

| Module | Production LOC |
|---|---:|
| `crates/norn-cli/src/runtime/mcp.rs` | 85 |
| `crates/norn/src/agent/mcp.rs` | 29 |
| `crates/norn/src/config/mcp.rs` | 368 |
| `crates/norn/src/config/mcp_approval.rs` | 215 |
| `crates/norn/src/config/mcp_local.rs` | 78 |
| `crates/norn/src/integration/mcp_http.rs` | 332 |
| `crates/norn/src/integration/mcp_runtime.rs` | 180 |
| `crates/norn/src/integration/mcp_stdio.rs` | 263 |
| `crates/norn/src/tools/agent/mcp_selection.rs` | 38 |
| `crates/norn/src/tools/agent/spawn_schema.rs` | 108 |

The already-over-limit legacy `spawn.rs` production prefix decreased from 755
to 673 lines by moving its static schema into `spawn_schema.rs`; this slice does
not grow that debt.

## Explicitly open

This candidate does not claim live add/remove/enable/disable/reload,
provider-visible tool refresh between requests, child-only connections beyond
the startup pool, dynamic MCP roots, approved-project activation coverage, HTTP
GET listening, reconnect/resumption, DELETE shutdown, OAuth,
resources/prompts/sampling, bounded response bodies, or bounded tool counts.
End-to-end root/spawn selection also needs a stronger fixture than the current
runtime and variant-resolution tests. These remain separately reviewable work
rather than being hidden inside the startup claim.
