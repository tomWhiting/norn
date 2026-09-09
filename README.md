# Norn

Norn is an AI agent runtime for interactive terminal work, command-line automation, and embedding in other applications. It can read and edit files, run commands, search code, use MCP tools, coordinate agents, and resume saved sessions. The Rust library, terminal UI, print mode, and driven JSON-RPC mode share the same `AgentBuilder` assembly path.

**Current source version: `0.1.0-preview.8`.** This is a development preview, not a stable release. See the [release notes](docs/release-notes/UNRELEASED.md) and [latest installation and verification record](docs/design/norn-retained-tui/briefs/NUI-005.md) for the tested scope and open findings.

## Install and start

Build from an up-to-date `main` checkout. The repository pins Rust **1.94.0** (edition 2024) in `rust-toolchain.toml`; building also requires access to the Git dependencies declared in `Cargo.toml` and locked in `Cargo.lock`.

```sh
git switch main
git pull --ff-only
cargo install --path crates/norn-cli --locked --force
norn --version
norn auth login
norn -C /absolute/path/to/project
```

Run these commands from the Norn repository. Cargo installs `norn` into `$CARGO_HOME/bin`, normally `~/.cargo/bin`; ensure that directory is on `PATH`. `command -v norn` shows which executable you are launching. `norn auth login --device-auth` supports sign-in without a local browser callback; `norn auth status` checks local credential state.

With no overrides, Norn selects **`gpt-6-astra`**, **high** reasoning, and an operating context window of **372,000 tokens** on its Codex subscription route. That window is Norn's operating default, not the model's maximum. Profiles, settings, and explicit overrides can change the selection. Model catalogue entries describe capabilities; your provider account still determines access.

```sh
norn --model gpt-6-astra --reasoning-effort high
norn -p "Explain this repository"
norn -r                         # resume the most recent session in this directory
norn --resume SESSION_ID
norn --fork SESSION_ID          # start a new session from saved context
norn --help
```

## MCP tools and Claude Code Channels-compatible push

Norn can start MCP servers and receive external messages from servers implementing the **ordinary Claude Code Channels message protocol over stdio**. This is a wire protocol, not a JavaScript dependency: the server can be written in Rust, JavaScript, or another language. An ordinary MCP tools server does not automatically support channel messages.

A channel server advertises `capabilities.experimental["claude/channel"] = {}` in its MCP initialization result, then sends JSON-RPC notifications such as:

```json
{
  "jsonrpc": "2.0",
  "method": "notifications/claude/channel",
  "params": {
    "content": "A new message arrived in the project room.",
    "meta": { "room_id": "project", "message_id": "123" }
  }
}
```

`content` is a string. Optional `meta` maps nonempty keys containing only ASCII letters, digits, and underscores to string values. Norn attributes input to the configured server connection; sender metadata cannot change that identity. Messages enter the session according to its channel policy without polling. Replying uses whatever ordinary MCP tools the server provides. Receiving a message neither grants tool approval nor acknowledges that the application has processed it.

### Launch with inline JSON

Replace the executable, arguments, environment, and working directory with those required by your server. The quotas below are **example choices**, not product defaults: 128 retained messages and 1,048,576 retained bytes across the channel inbox.

```sh
norn -C /absolute/path/to/project \
  --mcp-config '{"mcpServers":{"bridge":{"type":"stdio","command":"/absolute/path/to/channel-server","args":["--stdio"],"env":{"ROOM_ID":"project"}}}}' \
  --channel bridge=wake \
  --channel-max-retained-messages 128 \
  --channel-max-retained-bytes 1048576 \
  --channel-overflow reject-new
```

**The channel name must exactly match the `mcpServers` key.** In this example it is `bridge`, irrespective of the executable's name. `--channel mantle-go=wake` cannot select a server configured only as `mantle`.

`--mcp-config` also accepts a JSON file path. Repeat it for documents with distinct server names. Its root is exactly `{"mcpServers": {...}}`; each launch definition replaces the entire saved definition of the same name. Duplicate names/keys, unknown fields, and collisions with `--extension` are refused. Relative document and executable paths resolve from the effective `--working-dir`, not from the configuration file's directory. JSON process arguments are not shell commands and do not expand shell expressions.

### Save settings to use fewer flags

Add a `channels` member to your existing `~/.norn/settings.json` (or `$NORN_HOME/settings.json`). Preserve unrelated settings. Project `.norn/settings.json` and local `.norn/settings.local.json` can also supply channel settings.

This example enables optional wake delivery for channel-capable servers, using the same example quotas:

```json
{
  "channels": {
    "default_policy": "wake",
    "max_retained_messages": 128,
    "max_retained_bytes": 1048576,
    "overflow": "reject-new"
  }
}
```

You can then launch with just the server definition:

```sh
norn --mcp-config ./mcp-servers.json
```

`default_policy: "wake"` negotiates channels for enabled, approved stdio servers that advertise the capability. Ordinary servers retain their tools. An optional source that fails initialization or advertises a malformed capability is visibly excluded; healthy sources can continue. HTTP servers remain ordinary tools sources. Existing MCP approval still controls which servers may run.

For explicit source selection, replace `default_policy` with `"sources": {"bridge": "wake"}`. Named active sources are required: an unknown, disabled, non-stdio, failed, or non-channel server causes startup to fail. A named `"off"` excludes a known server even when default wake is enabled.

For a temporary launch override, pass the channel object directly:

```sh
norn --mcp-config ./mcp-servers.json \
  -c 'channels={"sources":{"bridge":"wake"},"max_retained_messages":128,"max_retained_bytes":1048576,"overflow":"reject-new"}'
```

`--mcp-config` loads server definitions, not a general settings file. `-c channels=JSON` supplies only the channel object. Channel precedence is **user < project < local < `-c channels=JSON` < dedicated flags**. Fields override lower values and source entries merge by name; an empty source map does not clear inherited entries. Restart to change channel policy or quotas: MCP reload retains the policy captured at startup.

### Policies, flags, and limits

| Option | Meaning |
| --- | --- |
| `--mcp-config JSON\|PATH` | Load complete MCP server definitions; repeatable. |
| `--extension NAME=stdio:///absolute/executable` | Short executable-only stdio definition; use `--mcp-config` for args/env or HTTP headers. |
| `--channel NAME=wake` | Allow idle TUI wakeup; busy work consumes input at a safe boundary. |
| `--channel NAME=next-turn` | Wait for an independently started interactive turn. |
| `--channel NAME=off` | Disable channel input from that known source, retaining its tools. |
| `--channel-max-retained-messages COUNT` | Positive total quota across retained input, including queued and claimed messages. |
| `--channel-max-retained-bytes BYTES` | Positive UTF-8 quota covering source labels, content, and metadata. |
| `--channel-overflow reject-new` | Visibly refuse new input when full while continuing MCP tool responses. |

Channels are disabled without an active policy. Active policies require both positive quotas and the explicit overflow action, supplied through settings or flags. `hold` is not exposed by the CLI: interactive inbox release/deny controls are not implemented.

In the **TUI**, `wake` can start a turn while idle without losing the composer draft. In **print and driven modes**, `wake` joins only the active run; it does not keep Norn alive after completion. `next-turn` is interactive-only. Ordinary message push is implemented; permission relay and live detach/reattach are separate work.

If startup reports `unknown MCP source`, check the exact JSON key. If it reports `server closed stdout`, the server exited or closed its MCP transport: check its executable, args, working directory, required environment, and server-side diagnostics. A withheld stderr line is not proof of the underlying cause.

See [MCP launch and Channels](docs/MCP-LAUNCH.md) for HTTP tool definitions, merge rules, approval boundaries, and detailed startup behaviour.

## Terminal UI

Colour detection never prevents interactive startup. `COLORTERM=truecolor` or `24bit` enables RGB; `TERM` names ending in `ghostty`, `kitty`, `alacritty`, or `wezterm`, and names containing `256color`, enable indexed colour without terminfo. Without explicit RGB evidence, other names use 16 ANSI colours; unset, empty, or `dumb` `TERM` uses the terminal’s default foreground and background. Reduced colour gets one notice inside the TUI, with selection and emphasis retained. Terminal I/O failures still report errors.

The TUI owns the screen, retains conversation history, and uses **Iridium** for its full-width composer. Tool rows show the tool name, supplied `tool_use_description`, and outcome compactly; click a row to inspect its details. The status line shows approximate context usage against the active configured window, separately from cumulative token usage.

| Command | Action |
| --- | --- |
| `/help` | List available slash commands; typing `/` opens completion. |
| `/model` | Open model selection. |
| `/pane` | Toggle the side pane. |
| `/pane diff` / `/pane agents` | Show changes or the agent tree. |
| `/view compact` / `/view detailed` | Choose global tool detail. |
| `/view follow` / `/view pin` | Follow the latest output or hold the reading position. |
| `/view composer send-key enter\|shift-enter\|alt-enter` | Choose which physical key sends a message. |
| `/view keys` | Inspect editable frontend shortcut bindings. |
| `/view preferences status` | Inspect active settings and their save status. |
| `/view help` | List scrolling, selection, search, copy, export, and layout controls. |

The pane shortcuts are **Option/Alt+P** (toggle), **Option/Alt+D** (diff), and **Option/Alt+A** (agents); **Option/Alt+S** cycles the send key. The terminal must report those modifiers. Bindings are editable with `/view keys set`, or in `tui.input.bindings` in settings. Enter sends by default; selecting Shift+Enter or Alt+Enter lets bare Enter insert newlines.

Frontend changes save to personal settings by default. Use `/view preferences run` for temporary changes or `/view preferences local` to save workspace-local preferences. See [TUI preferences](docs/TUI-PREFERENCES.md) for JSON examples, shortcuts, precedence, and save conflicts.

## Automation and structured output

Print mode can produce text, one JSON result, or streaming JSON events:

```sh
norn -p --output-format json "Summarize this repository"
norn -p --output-format stream-json --partial "Review the current changes"
norn -p --output-schema ./result.schema.json "Return a result matching this schema"
```

For bidirectional integration:

```sh
norn --protocol jsonrpc --mcp-config ./mcp-servers.json
```

The peer sends `initialize`, then one `run/execute`; Norn streams `event/*` notifications and returns the final result. Stdout contains protocol messages and stderr contains logs. Saved channel settings and the same channel flags apply. This is a single-run protocol, not an idle daemon or live session attachment endpoint. See the [driven-mode guide](docs/DRIVEN-MODE-GUIDE.md) and [wire contract](docs/design/norn-cli/DRIVEN-PROTOCOL.md).

Use `norn session --help`, `norn auth --help`, `norn mcp --help`, and `norn completion --help` for subcommand details. `norn doctor` checks setup; `norn mcp serve` exposes Norn as an MCP server. Alternative provider configuration is described in [provider backends](docs/provider-backends.md).

## Library and development

| Crate | Responsibility |
| --- | --- |
| `norn` | Agent runtime, tools, providers, sessions, rules, and configuration. |
| `norn-cli` | The `norn` binary, print and driven modes, and subcommands. |
| `norn-tui` | Terminal rendering, input, and frontend preferences. |
| `norn-macros` | Tool argument schemas and follow-up derive macros. |

Embedders construct an `AgentBuilder` and run the resulting agent. [Library examples](crates/norn/examples) include `chat.rs`, `login.rs`, and `smoke.rs`. See the [documentation index](docs/DOCUMENTATION.md) for architecture and design material.

```sh
cargo build --workspace
cargo run --bin norn -- --help
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

The repository requires clean strict lint and formatting checks; see [CLAUDE.md](CLAUDE.md) for contribution standards.

## License

[AGPL-3.0](LICENSE).
