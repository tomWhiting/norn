# MCP launch configuration and Channels

5 September 2026, Melbourne time. This guide describes the startup interface available in this source under [NML-001](design/norn-mcp-launch/briefs/NML-001.md).

## Source availability and historical installation

At 18:09 Melbourne, `/Users/tom/.cargo/bin/norn` was updated to binary SHA-256 `450bb7a890045669068861abbeda803cd373ef8094603c23a314929a44d09684`. It was built from the uncommitted NMS-002 owner-default changes over main `911eddd07401032a8ce0883392aea7b38ee98fc2`: GPT-6 Astra, high reasoning where the selected route supports it, and Astra's 372,000-token operating window. Explicit settings, profile and CLI overrides remain effective.

The native build and the focused `astra_operating_defaults_and_explicit_settings_reach_cli_assembly` test passed. The six-leg 205 receipt belongs to exact commit `911eddd`, before these defaults. It does not cover the changed defaults or the new MCP launch implementation. This is a dated installation checkpoint, not the installed identity of NML-001. No new committed release or Fable pass is claimed. Local proof: `/private/tmp/nwp-03-proof/astra-defaults-install/installation.json` and `defaults-test.json`.

Ordinary Claude-style Channels message push is implemented. Norn accepts the experimental `claude/channel` capability and `notifications/claude/channel` frames from enabled stdio sources. Real Rust stdio and TUI fixtures passed; Norn does not require a JavaScript runtime. Those results do not certify every MCP server or prove the live Cambium/Hammerbarn bridge. Optional permission relay and a persistent attachable daemon remain separate work. A channel message never grants tool approval.

The installed build accepts repeatable `--extension NAME=stdio:///absolute/executable` and HTTP(S) endpoint forms. The stdio URI form supplies an executable only; saved MCP definitions support arguments, environment and headers. **The `--mcp-config` interface below is available in this source. It is absent from historical binary `450bb7a`; the final candidate receipt and installation identity are recorded separately in the programme evidence.**

## Inline JSON or a file

The repeatable flag is `--mcp-config JSON|PATH`. Its document root is exactly an `mcpServers` object mapping server names to complete definitions. An empty map is valid. For a stdio server:

```sh
norn --mcp-config '{"mcpServers":{"bridge":{"type":"stdio","command":"/absolute/channel-server","args":["--stdio"],"env":{"CHANNEL_ROOM":"work"}}}}'
```

`command`, `args` and `env` are process data. Norn does not interpret them as a shell command or expand shell expressions inside JSON. Replace the example executable and arguments with the server's actual interface.

A file can hold a remote definition, including headers:

```json
{
  "mcpServers": {
    "remote-tools": {
      "type": "http",
      "url": "https://mcp.example.test/mcp",
      "headers": {"Authorization": "Bearer <server-access-token>"}
    }
  }
}
```

The URL and token above are placeholders. Protect a file containing real credentials appropriately, then pass its path:

```sh
norn --working-dir /absolute/project --mcp-config ./mcp-remote.json
```

Document paths and relative server-command paths resolve from the effective `--working-dir`. They do not become relative to the JSON file's directory. Repeat the flag to add documents with disjoint server names.

## Precedence and refusal rules

Launch documents and named `--extension` entries form the existing CLI configuration overlay, above saved configuration layers. A CLI definition replaces the complete lower-layer definition with the same name; omitted fields are not silently inherited from that saved definition. `{"mcpServers":{"bridge":{"enabled":false}}}` masks a lower-layer server for this launch. Runtime configuration survives the existing MCP reload path but is not written to settings files.

Duplicates are refused, rather than choosing a winner: duplicate JSON keys at any depth, repeated server names across documents, and name collisions with `--extension`. Unknown fields, malformed documents, unsupported transports and incompatible transport settings are refused before an MCP subprocess starts. The standard `type` field maps to the existing `transport` field; supplying both is a duplicate declaration and is refused. Existing typed settings validation still applies to arguments, environment, headers, enabled state and declared limits.

An invalid MCP document causes an ordinary startup argument error with exit code 2. TUI startup preserves typed build failures: argument errors exit 2 and authentication errors exit 3, instead of collapsing them into generic agent exit 1. These failures occur before terminal setup; a real public-entry subprocess test covers the invalid-document exit-2 path. In driven mode, document resolution after acceptance of `run/execute` returns its matching error response before exit; it does not start a sentinel MCP process. This is distinct from malformed command-line syntax rejected before the RPC loop starts. Raw inline documents, environment values and header values must not appear in parse errors or CLI Debug output. Command-line arguments remain locally observable; use a protected file rather than inline credentials when that matters.

## Explicit channel admission

Registering an MCP server does not enable it as a channel. Choose the source policy, both positive retained-message limits and an overflow action explicitly:

```sh
norn --mcp-config "$MCP_CONFIG_JSON" \
  --channel bridge=wake \
  --channel-max-retained-messages "$MAX_MESSAGES" \
  --channel-max-retained-bytes "$MAX_BYTES" \
  --channel-overflow reject-new
```

Set `MCP_CONFIG_JSON` to the document and both limits to the values chosen for the workload. No operational quota is supplied by this example. HTTP headers configure remote tool access; ordinary channel message push remains the stdio capability described above.

| Launch mode | Supported CLI policy | Lifetime |
|---|---|---|
| Interactive TUI | `wake`, `next-turn` | `wake` can start a turn while idle, preserving the draft; `next-turn` waits for an independently submitted turn. |
| Print | `wake` | During the active run only. |
| Driven JSON-RPC | `wake` | During the single accepted `run/execute` only. |

CLI `hold` remains unavailable until inspect/release/deny controls exist. Reply tools remain ordinary MCP tools; receiving a notification is not an application-processing acknowledgement.

## Driven startup

Use the same launch configuration with the existing protocol:

```sh
norn --protocol jsonrpc --mcp-config ./mcp-bridge.json \
  --channel bridge=wake \
  --channel-max-retained-messages "$MAX_MESSAGES" \
  --channel-max-retained-bytes "$MAX_BYTES" \
  --channel-overflow reject-new
```

The peer still sends `initialize` and one `run/execute` with its prompt. MCP definitions are launch flags, not new request parameters. They add no dynamic MCP mutation method, permission-relay endpoint, idle daemon or second run. JSON-RPC stdout stays protocol-only. See [the driven contract](design/norn-cli/DRIVEN-PROTOCOL.md).

Twelve parser tests and six real-process cases passed natively. The process cases cover inline print launches and preserved disk settings, relative document/executable paths in driven mode with active channel events and one-shot exit, interactive reload retention, id-matched driven refusal, print exit 2 and public TUI startup exit 2 before terminal setup or MCP launch. Formatting and strict release-profile workspace/all-targets Clippy, including the live-smoke feature, also passed. These are local diagnostics, not a full-suite or live-provider claim. The exact candidate battery, source review and installed-artifact checks remain separate evidence; final candidate and installation identities belong in the external programme proof, not a rewrite of this frozen guide.
