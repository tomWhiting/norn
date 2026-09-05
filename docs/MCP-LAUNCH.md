# MCP launch configuration and Channels

5 September 2026, Melbourne time. This guide describes the startup interface available in this source under [NML-001](design/norn-mcp-launch/briefs/NML-001.md).

## Historical installation checkpoint

The historical NML-001/NV-002 installation at 20:15 Melbourne on 5 September 2026 used exact commit `5227db49d805a4c0729912dca3832c842c8f39e1`, installed at 20:15:11 Melbourne on 5 September 2026 and merged to local/remote main. Installed SHA-256: `dc61b730918db4928e5e9ff0cf1194f7ed0fdfcf0d010286ab87f8d2d26e0a19`. Its exact 205 battery passed all six declared legs with 14 venue bindings; native build and installed help/list/redacted-error checks passed. Proof: `/private/tmp/nwp-04-proof/exact-battery-2/execution-result.json` and `/private/tmp/nwp-04-proof/native-release-2/installation.json`. Fresh Fable re-review remains pending; no new review pass or waiver is claimed.

The historical `3964799` battery remains red: five green legs and tests exit 101. Its detailed cancellation error was not retained. NV-002 changed only two test cleanup waits; the green repaired-candidate receipt does not identify the old failure cause or relabel the old result. The earlier 18:09 `450bb7a` defaults installation and the `911eddd` receipt remain historical.

Ordinary Claude-style Channels message push is implemented. Norn accepts `claude/channel` and `notifications/claude/channel` from enabled stdio sources; Rust stdio and TUI fixtures passed without a JavaScript runtime requirement. Tom reported a successful real Hammerbarn inline launch on 5 September 2026. That owner report is distinct from captured fixture/receipt evidence and does not certify live Cambium or every adapter. Optional permission relay and persistent attachment remain separate. A channel message never grants tool approval.

That recorded build includes `--mcp-config JSON|PATH` and `--extension`. The stdio URI form of `--extension NAME=stdio:///absolute/executable` supplies an executable only; full definitions carry args/env/headers. **Persisted channel policy, `-c channels=JSON`, optional default delivery and named `off` are implemented in this source under [NCS-001](design/norn-channel-settings/briefs/NCS-001.md). Verification is in progress. The historical `5227db4` build predates these settings features; its receipt does not verify NCS-001. Exact candidate, check results and installation identity are tracked in `/private/tmp/ncs-001-proof` and the external Norn programme.**

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

## Channel settings

The root `channels` object uses the existing files: user `~/.norn/settings.json` (or `$NORN_HOME/settings.json`), project `<working-dir>/.norn/settings.json`, and local `<working-dir>/.norn/settings.local.json`. Add it to the existing document; do not replace unrelated settings. The MCP `mcpServers` envelope remains exclusive to `--mcp-config`; it is not a general settings loader.

For illustration, an operator who chooses a maximum of **32 retained messages** and **65,536 retained bytes** could write the following. These are chosen example limits, not product defaults or recommended workload values. Choose limits for your own workload before applying the example. `quiet-tools` must name an actual configured server.

```json
{
  "channels": {
    "default_policy": "wake",
    "sources": {"quiet-tools": "off"},
    "max_retained_messages": 32,
    "max_retained_bytes": 65536,
    "overflow": "reject-new"
  }
}
```

`default_policy` and each named source accept `off`, `wake` or `next-turn`. No channel section means disabled; absent default policy means `off`. An active delivery selection requires both positive limits and explicit `reject-new`, assembled from settings or flags. Inactive settings may retain limits for later use; all-off selection constructs no channel runtime. `hold` is not accepted by this settings/CLI interface because inspect/release/deny controls are not present.

Channel fields merge in this order: **user < project < local < `-c channels=JSON` < dedicated channel flags**. Present fields replace lower values, and source entries merge by name. Named `off` overrides lower delivery for that name. An empty source map does not clear inherited entries; missing or null optional fields do not override them. This differs from MCP launch definitions, which replace each complete named server entry.

The same object can be supplied at launch. Set both shell variables below to the positive limits you choose; no numeric quota is supplied by this command:

```sh
norn --mcp-config "$MCP_CONFIG_JSON" \
  -c "channels={\"sources\":{\"bridge\":\"wake\"},\"max_retained_messages\":${MAX_MESSAGES},\"max_retained_bytes\":${MAX_BYTES},\"overflow\":\"reject-new\"}"
```

When saved settings already contain policy and limits, dedicated flags are unnecessary. A flag can override just one saved value; `--channel bridge=off` excludes that named source while leaving other merged entries intact. Repeating `-c channels=...`, duplicate fields/source names, unknown fields/policies and zero limits are refused by name. Raw inline JSON is withheld from errors and Debug output.

Default delivery is **optional negotiation for enabled, approved stdio sources**: a server advertising a valid `claude/channel` capability participates; an ordinary server without it keeps its tools. If an optional source fails initialization or capability validation, that server has a visible failure and is excluded while healthy sources can still publish. A malformed advertised capability always fails that connection; it is never accepted as an ordinary tools-only connection. Because default selection is optional, that connection failure does not fail the entire candidate. The listener is installed before initialization so the first notification is not lost. No provider request, extra subprocess, polling or history scan is added for discovery.

Named `wake`/`next-turn` is required: an unknown, disabled or HTTP source, a missing/malformed capability, or an initialization failure is fatal to the candidate instead of silently omitting the requested source. Named `off` can exclude a disabled or HTTP definition but still refuses an unknown name. A default policy leaves HTTP sources as ordinary tools. Existing project MCP approval remains the authority to run a server; channel settings cannot approve it, widen its tool access or bypass generation fencing.

**Changing channel policy or limits requires restart.** MCP reload refreshes definitions under the policy captured at startup; it does not reread policy files into a running session. Effective policy is checked after all layers merge and against the actual dispatch mode, including TUI fallback. `next-turn` is interactive-only; print and driven accept active-run `wake` or `off`. Driven still accepts one run and exposes no idle listener or live policy-mutation method.

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

## Recorded verification

At the NML-001 checkpoint, twelve parser tests and six real-process cases passed natively. The process cases cover inline print launches and preserved disk settings, relative document/executable paths in driven mode with active channel events and one-shot exit, interactive reload retention, id-matched driven refusal, print exit 2 and public TUI startup exit 2 before terminal setup or MCP launch. Formatting and strict release-profile workspace/all-targets Clippy, including the live-smoke feature, also passed. These are local diagnostics, not a full-suite or live-provider claim. Those NML diagnostics are supplemented by the historical exact `5227db4` receipt and installation above. They do not verify NCS-001. The settings implementation is undergoing typed-layer, stdio/process, strict lint, source-review and exact candidate verification. `/private/tmp/ncs-001-proof` and the external programme record the actual completed checks and any later installation. This source guide does not declare an NCS battery pass or installed-artifact result; fresh external review is recorded separately.
