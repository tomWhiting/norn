---
type: design
cluster: norn-cli
title: "Norn CLI: Agent Command-Line Interface"
---

# Norn CLI: Agent Command-Line Interface

## Intention

Norn CLI puts the Norn headless agent runtime into the hands of a human at a terminal. It is the thinnest possible shell around libnorn: it handles input, output, authentication, session persistence, and nothing else. The agent logic, tool execution, schema enforcement, and everything interesting lives in the library crate. The CLI is a consumer, not an extension of the runtime.

Two modes of operation serve two audiences. Print mode (`-p`) runs a single step, produces structured or textual output, and exits. Interactive mode starts a reedline REPL with slash commands, streaming tool display, and session continuity across turns. Both modes share every flag, every capability, and the same underlying `run_agent_step` call. The difference is input handling and output rendering.

## Problem

Norn is a library crate. It has no binary, no way to invoke it from a terminal, and no session persistence. Testing requires writing Rust code and compiling an example binary. There is no way to:

- Run an agent from the command line with a prompt and get structured output
- Interactively chat with an agent, observing tool calls as they happen
- Resume a previous conversation
- Pipe data through an agent as part of a shell pipeline
- Specify output schemas, models, profiles, or tool sets via CLI flags

The existing example binaries (`smoke.rs`, `chat.rs`, `login.rs`) prove the runtime works but are not production tools. They lack session persistence, streaming display, slash commands, profile loading, and every convenience a real CLI needs.

## Solution

### NC1: Single binary, two modes

The binary is `norn`. The crate is `norn-cli`. Default mode is interactive REPL when both stdin and stdout are TTYs. Print mode (`-p` / `--print`) forces non-interactive execution. Auto-detection: piped stdin or piped stdout implies print mode without requiring the flag.

```
norn                              # Interactive REPL
norn "prompt"                     # REPL with initial message
norn -p "prompt"                  # Print mode (headless)
echo "input" | norn               # Print mode (piped stdin)
norn "prompt" | jq .              # Print mode (piped stdout)
```

### NC2: Command tree

Top-level subcommands exist only for operations genuinely distinct from talking to the agent:

```
norn [FLAGS] [PROMPT...]          # Agent interaction (REPL or print)
norn session <SUBCOMMAND>         # Session management
norn auth <SUBCOMMAND>            # Authentication
norn mcp <SUBCOMMAND>             # MCP server operations
norn doctor                       # Health check
norn completion <SHELL>           # Shell completion scripts
```

No `norn run` subcommand. No `norn tools` subcommand. No `norn profile` subcommand (profile management is premature until there is concrete functionality beyond `--profile`). Tool discovery lives in the `/tools` slash command. Profile inspection is `--profile <name>` plus `/profile` in the REPL.

### NC3: Agent configuration flags

These flags configure the agent step. All are available in both REPL and print modes.

```
-m, --model <MODEL>               Model identifier (overrides profile)
    --profile <PATH|NAME>         Profile to load (TOML/JSON file, or name
                                  resolved from ~/.config/norn/profiles/)
-S, --system-prompt <TEXT>        System prompt (overrides profile instructions)
    --append-system-prompt <TEXT>  Append to system prompt (additive)
    --allowed-tools <PATTERNS>    Tool allow-list, comma-separated
    --disallowed-tools <PATTERNS> Tool deny-list, comma-separated
    --reasoning-effort <LEVEL>    none | low | medium | high | xhigh | max
    --max-turns <N>               Maximum provider round-trips per step
    --timeout <DURATION>          Step timeout (e.g. 2m, 30s)
-C, --working-dir <DIR>           Working directory for tool execution
-c, --config <KEY=VALUE>          Config override, repeatable
    --rules <PATH>                Rules YAML file
    --variables <KEY=VALUE>       Session variable ({{key}} expansion), repeatable
-e, --extension <URI>             Connect MCP extension, repeatable
```

Config overrides (`-c`) map to `AgentLoopConfig` and `ProviderConfig` fields: `timeout`, `max_turns`, `schema_budget`, `context_window`, `compact_threshold`, `compact_keep_turns`, `retry_max`, `retry_base_delay`.

### NC4: Output control flags

```
-s, --output-schema <JSON|PATH>   JSON Schema for structured model output.
                                  Inline JSON if value starts with {,
                                  otherwise treated as a file path.
    --event-schema <TYPE=JSON|PATH>  Per-event-type schema, repeatable.
                                  TYPE is one of: assistant_message,
                                  spoken_response, tool_call_envelope,
                                  stop_output, question, handoff, review,
                                  progress.
-f, --output-format <FORMAT>      CLI rendering format:
                                  text (default) | json | stream-json
-o, --output <PATH>               Write final output to file
-q, --quiet                       Suppress progress/tool output on stderr
```

The separation is explicit: `-s` controls what the MODEL produces (schema enforcement via the structured-output tool). `-f` controls how the CLI renders output. `--event-schema` maps directly to Norn's `EventSchemaSet` with per-`EventType` schemas.

### NC5: Mode control

```
-p, --print                       Non-interactive print mode
```

Matches the Pi and Claude Code convention where `-p` = print/headless. Mode detection logic:

1. If `--print` is set: print mode.
2. If stdin is not a TTY (piped input): print mode.
3. If stdout is not a TTY (piped output): print mode.
4. Otherwise: interactive REPL.

### NC6: Session control flags

```
-r, --resume [ID|NAME]            Resume a session (no arg = most recent)
    --fork [ID|NAME]              Fork a session (no arg = most recent)
    --no-session                  Don't persist this session
```

Sessions are persisted as JSONL files in `~/.local/share/norn/sessions/` (XDG-compliant). Each session file contains serialized `SessionEvent` records. An index file enables fast listing and lookup by ID or name.

### NC7: Stdin handling

No `--stdin` flag. Piped input is auto-detected via `stdin.is_terminal()`.

When stdin is piped, all content is read before execution begins. If a positional PROMPT is also provided, stdin content is prepended as context with a delimiter:

```
<stdin>
{stdin content here}
</stdin>

{positional prompt here}
```

When stdin is a TTY, it is used for the REPL's line editor input.

### NC8: Session persistence

The `EventStore` is currently in-memory only. The CLI adds a persistence layer:

- **Format**: JSONL (one `SessionEvent` per line, JSON-serialized).
- **Location**: `~/.local/share/norn/sessions/{id}.jsonl` where `id` is a UUID v7 (time-sortable).
- **Index**: `~/.local/share/norn/sessions/index.jsonl` with per-session metadata (id, name, model, working directory, created/updated timestamps, event count, status).
- **Write protocol**: append-only. Events are flushed after each `run_agent_step` call. The index entry is updated atomically.
- **Resume**: loads all events from the JSONL file, reconstructs the `EventStore` and message history, and continues the conversation.
- **Fork**: copies the source session's events into a new session, appends a `Fork` event, and continues independently.
- **Names**: sessions can be named via `/name <text>` slash command or `--session-name <text>` flag.

### NC9: Interactive REPL

The REPL uses reedline for input. reedline is chosen because the REPL does not use ratatui's alternate screen: output is rendered inline using standard terminal writes, so the line editor does not conflict with the rendering model.

REPL features:
- Multi-line input (Shift+Enter or a configurable key for newlines)
- History persisted to `~/.local/share/norn/history.txt`
- Tab completion for slash commands, file paths, tool names, and model names
- Prompt shows model name and session status
- Ctrl+C interrupts the current agent step (cancels the provider call)
- Ctrl+D exits the REPL

### NC10: Slash commands

Slash commands work in both REPL and print mode. In print mode, the prompt is preprocessed through `preprocess_input()` before execution. In REPL mode, every user input is preprocessed.

Built-in slash commands:

```
/help                             Show available commands and flags
/tools                            List available tools with descriptions
/model [NAME]                     Show current model or switch model
/schema [JSON|PATH]               Show or set output schema
/compact                          Compact conversation context (real compaction
                                  via ContextEdits, not a model request)
/clear                            Clear conversation history (new EventStore)
/session                          Show current session info (id, name, turns,
                                  token usage)
/name <TEXT>                       Name the current session
/variables                        List active session variables
/exit, /quit                      Exit the REPL
```

Additional slash commands can be registered by profiles via the `SlashCommandRegistry` that Norn already provides. The CLI registers its own built-ins, then the profile can add more.

### NC11: Streaming display

In REPL mode, the CLI subscribes to the `broadcast::Sender<ProviderEvent>` that `run_agent_step` accepts. A display task renders events as they arrive:

- **Text deltas**: streamed to stdout character-by-character.
- **Thinking deltas**: rendered dimmed/italic on stderr.
- **Tool call deltas**: accumulated until the tool call is complete, then displayed as a summary line (e.g., `> bash: find crates/norn/src/ -name '*.rs' | wc -l`).
- **Tool results**: displayed with head/tail truncation (first 10 + last 10 lines for long output, full output behind a toggle or scroll).
- **Schema validation**: feedback shown on stderr when a schema attempt fails and retries.
- **Done**: usage summary (tokens in/out, elapsed time).

In print mode with `--output-format text`, tool progress is written to stderr and the final output to stdout. With `json`, a single JSON object is written to stdout at completion. With `stream-json`, NDJSON events are written to stdout as they arrive.

### NC12: Profile integration

The CLI's profile loading path:

1. If `--profile <path>` is a file path (contains `/` or `.`): load directly via `Profile::from_file()`.
2. If `--profile <name>` is a bare name: resolve from `~/.config/norn/profiles/{name}.toml` (or `.json`).
3. If no `--profile`: use a minimal default profile with no tools gated and a generic system prompt.

CLI flags override profile values: `--model` overrides `profile.model`, `--system-prompt` overrides `profile.system_instructions`, `--allowed-tools` overrides `profile.tools`, `--reasoning-effort` overrides `profile.reasoning_effort`.

Per-event schemas from the profile's `event_schemas` section are loaded into `EventSchemaSet` and threaded through `LoopContext`. CLI `--event-schema` flags are additive (merged on top of profile schemas).

### NC13: Auth subcommand

```
norn auth login [--codex-home <DIR>]    OAuth PKCE login (opens browser)
norn auth logout                         Clear stored credentials
norn auth status                         Show auth state: logged in,
                                         token expiry, account ID
```

Delegates to Norn's existing `login()` and `logout()` functions from `provider::auth`. Status reads `~/.codex/auth.json` and reports without exposing tokens.

### NC14: Session subcommands

```
norn session list [--all] [--limit <N>] [--format table|json]
norn session show <ID|NAME>
norn session resume <ID|NAME>
norn session fork <ID|NAME>
norn session export <ID|NAME> [--format jsonl|json|markdown]
norn session remove <ID|NAME>
```

`session list` defaults to sessions from the current working directory. `--all` shows all sessions. Sessions are listed newest-first. ID can be a partial prefix (minimum 8 characters for disambiguation).

`session resume` and `session fork` are convenience wrappers equivalent to `norn --resume <ID>` and `norn --fork <ID>`.

### NC15: MCP subcommands

```
norn mcp serve                    Start Norn as an MCP server on stdio
norn mcp connect <URI>            Test connection to an MCP server
```

`mcp serve` uses the existing `mcp_server::serve_stdio()` implementation with the full tool registry. `mcp connect` performs an `initialize` handshake and `tools/list` to verify the server is reachable and report its capabilities.

### NC16: Doctor subcommand

```
norn doctor
```

Checks:
- OAuth status (logged in, token validity)
- Provider connectivity (can reach the API endpoint)
- Profile validity (if a default profile exists)
- Working directory permissions
- Tool availability (tree-sitter grammars, LSP binaries)

Reports pass/fail for each check with actionable remediation messages.

### NC17: Shell completions

```
norn completion bash
norn completion zsh
norn completion fish
```

Generates shell completion scripts via clap's `clap_complete` integration. Outputs to stdout for redirection to the appropriate shell config file.

### NC18: Output format semantics

Three formats, each with defined semantics:

**text** (default): Human-readable. In print mode, the final model output is written to stdout. If an output schema was set, the structured JSON is pretty-printed. Tool calls and progress appear on stderr unless `--quiet` is set.

**json**: Machine-readable envelope. A single JSON object is written to stdout after completion:
```json
{
  "output": <model output value>,
  "usage": {"input_tokens": N, "output_tokens": N},
  "model": "model-name",
  "session_id": "uuid",
  "events": [<SessionEvent>, ...],
  "result": "completed|schema_unreachable|max_iterations|timed_out"
}
```

**stream-json**: NDJSON streaming. One JSON object per line, written to stdout as events arrive:
```json
{"type": "text_delta", "text": "..."}
{"type": "tool_call", "id": "...", "name": "...", "arguments": "..."}
{"type": "tool_result", "id": "...", "output": {...}}
{"type": "thinking_delta", "text": "..."}
{"type": "completed", "output": {...}, "usage": {...}}
```

### NC19: Multi-turn conversation in REPL

The REPL maintains a single `EventStore` across turns. Each user message triggers a new `run_agent_step` call with the accumulated conversation history. The `messages` vector is rebuilt from the store's events before each call, ensuring session context carries forward.

Between turns, `LoopContext::clear_dynamic_sections()` resets rule injections, and `evaluate_prompt_commands()` refreshes dynamic system sections. The system instruction is reconstructed from sections each iteration.

### NC20: Config override mapping

The `-c key=value` flag maps to specific runtime fields:

| Key | Maps to | Type |
|-----|---------|------|
| `timeout` | `AgentLoopConfig::step_timeout` | Duration |
| `max_turns` | `AgentLoopConfig::max_iterations` | u32 |
| `schema_budget` | `AgentLoopConfig::schema_attempt_budget` | u32 |
| `context_window` | `AgentLoopConfig::context_window_limit` | u64 |
| `compact_threshold` | `AgentLoopConfig::auto_compact_threshold_pct` | f64 |
| `compact_keep_turns` | `AgentLoopConfig::auto_compact_keep_recent_turns` | usize |
| `base_url` | `ProviderConfig::base_url` | String |
| `max_retries` | `ProviderConfig::max_retries` | u32 |
| `request_timeout` | `ProviderConfig::timeout` | Duration |
| `retry_max` | `RetryPolicy::max_retries` | u32 |
| `retry_base_delay` | `RetryPolicy::initial_backoff` | Duration |
| `provider_options` | `ProviderConfig::provider_options` | JSON |

Note: `max_retries` is HTTP-level retry (ProviderConfig). `retry_max` is loop-level transient-error retry (RetryPolicy). These are distinct mechanisms at distinct layers.

Unknown keys produce a warning and are ignored.

### NC21: Token estimation and auto-compaction wiring

The CLI unconditionally wires `SimpleTokenEstimator` into `LoopContext::token_estimator` and `ContextEdits::new()` into `LoopContext::context_edits`. Without these, `context_window_limit`, `auto_compact_threshold_pct`, and the `/compact` slash command cannot function. These are not optional: they are required infrastructure for any session longer than a few turns.

### NC22: Diagnostics collection

The CLI constructs a `DiagnosticCollector` and makes it available during agent execution. At step completion:

- In `text` format: non-empty diagnostics are rendered on stderr with severity, code, message, and suggestion.
- In `json` format: diagnostics are included in the output envelope under a `diagnostics` key.
- In `stream-json` format: diagnostics are emitted as `{"type": "diagnostic", ...}` events.

### NC23: Provider selection

The CLI supports two providers via profile or flag:

- `OpenAiProvider` (default): OAuth via codex-login, Responses API.
- `ClaudeRunnerAdapter`: routes through Claude Code CLI, configured via `--provider claude-runner` or profile `provider: claude-runner`.

The `--provider` flag or profile `provider` field selects the backend. Default is `openai`.

### NC24: Iteration monitoring via profile

`IterationMonitorConfig` fields are configured via profile, not CLI flags (they are pre-configured operational parameters, not per-invocation choices):

```toml
[iteration_monitor]
context_window_tokens = 200000
warn_threshold_pct = 0.75
handoff_threshold_pct = 0.90
handoff_guidance = "Context is nearly full. Summarise findings and complete."
failure_repeat_window = 3
hedging_patterns = ["I cannot", "I'm unable"]
```

The CLI loads these from the profile and threads them into `LoopContext::iteration_monitor`.

## Non-Goals

### NG1: Full TUI

The CLI does not build a ratatui alternate-screen TUI. The REPL renders inline using standard terminal output. A full TUI is a separate future crate (`norn-tui`) that would share the same libnorn interface.

### NG2: Client-server architecture

The CLI runs the agent in-process. There is no app-server, no WebSocket, no daemon. The agent loop executes in the same process as the input/output handling. A client-server split is a future consideration for remote operation.

### NG3: Approval/permission system

The CLI does not implement tool-call approval prompts. All tool calls execute immediately. An approval mechanism (channel-based pause in `ToolExecutor`) is a future addition to libnorn itself, not a CLI concern.

### NG4: Plugin/marketplace system

No plugin discovery, installation, or marketplace integration. Extensions are connected via `--extension <URI>` pointing to MCP servers.

### NG5: Desktop notifications

No system notification integration. The terminal is the notification surface.

## Structure

```
crates/norn-cli/
  src/
    main.rs                       -- entry point, clap parse, mode dispatch
    lib.rs                        -- 7 pub mod declarations

    cli/
      mod.rs                      -- re-exports
      args.rs                     -- Cli struct, clap argument tree, value enums
      mode.rs                     -- REPL vs print mode detection
      exit.rs                     -- ExitCode enum (0/1/2/3 mapping)
      error.rs                    -- BuildError (thiserror, drives exit codes)

    config/
      mod.rs                      -- re-exports
      assembly.rs                 -- ConfigOverrides, ProviderConfigOverrides parsing
      profile_loader.rs           -- --profile resolution (path or name lookup)
      overrides.rs                -- CLI flag → Profile mutation
      variables.rs                -- --variables → VariableStore
      event_schemas.rs            -- --event-schema + profile schema merging
      extensions.rs               -- --extension URI collection
      rules.rs                    -- --rules YAML loading → RuleEngine
      paths.rs                    -- XDG directory resolution (CO2)

    runtime/
      mod.rs                      -- re-exports
      from_cli.rs                 -- builder_from_cli (assembly via norn AgentBuilder;
                                     the former builder.rs/bundle.rs parallel assembly
                                     stack was deleted by the R1 unification)
      resolve.rs                  -- CLI flag/config resolution
      wiring.rs                   -- ToolExecutor/SlashState wiring off AgentParts

    print/
      mod.rs                      -- re-exports run()
      orchestrator.rs             -- end-to-end print-mode driver (NC-003)
      output.rs                   -- text/json/stream-json output formatters
      provider.rs                 -- OpenAI/ClaudeRunner provider construction

    repl.rs                       -- placeholder REPL (NC-005 replaces with repl/)

    commands/
      mod.rs                      -- subcommand dispatcher re-exports
      auth.rs                     -- norn auth login/logout/status
      completion.rs               -- norn completion bash/zsh/fish
      doctor.rs                   -- norn doctor health checks
      mcp.rs                      -- norn mcp serve/connect
      session.rs                  -- norn session list/show/resume/fork/remove
      session_export.rs           -- norn session export (jsonl/json/markdown)
      slash/
        mod.rs                    -- slash command re-exports
        registry.rs               -- SlashCommandRegistry, builtin registration
        dispatch.rs               -- dispatch_input, preprocess_input
        actions.rs                -- /compact, /clear, /model, /schema handlers
        state.rs                  -- SlashState (mutable session state for commands)

    session/
      mod.rs                      -- JSONL persistence re-exports
      types.rs                    -- SessionIndexEntry, SessionStatus, SessionPersistError
      io.rs                       -- JSONL read/write, index operations
      ops.rs                      -- create_session, resume_session, fork_session
      tests.rs                    -- session persistence tests

  tests/
    build_runtime_integration.rs  -- end-to-end runtime assembly tests

  Cargo.toml
```

## Constraints

### CO1: Thin wrapper

The CLI crate depends on `norn` (libnorn) and adds no agent logic. All intelligence lives in the library. The CLI handles I/O, persistence, and UX.

### CO2: XDG compliance

Data files in `~/.local/share/norn/`, config files in `~/.config/norn/`, cache in `~/.cache/norn/`. Respect `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME` overrides. On macOS, fall back to `~/Library/Application Support/norn/` when XDG vars are not set.

### CO3: No unwrap in library paths

CLI code (binary entry, arg parsing, display) may use `expect()` with descriptive messages at startup. All libnorn calls go through proper error handling.

### CO4: Streaming first

The CLI must display streaming output from the first token. It cannot buffer the entire response before displaying. The `broadcast::Sender<ProviderEvent>` channel is the streaming interface.

### CO5: Piping compatibility

Print mode output must be valid for shell piping. stdout contains only the result (text, JSON, or NDJSON). stderr contains progress and diagnostics. `--quiet` suppresses stderr. Exit codes: 0 = success, 1 = agent error, 2 = CLI argument error, 3 = auth error.
