# Norn

Headless, embeddable AI agent runtime. The `norn` library is the engine; a CLI, a TUI, and a JSON-RPC driven mode are thin drivers over it.

## What it is

Norn gives AI agents the ability to read and write files, run commands, search code, coordinate with other agents, and persist their work across sessions. It is designed to be embedded as a library: an embedder constructs an `AgentBuilder`, calls `build()`, and runs the agent. [Meridian](https://ablative.com.au) is the flagship embedder, using Norn as the runtime behind its AI assistant sessions.

Every interactive front-end — the `norn` CLI, the terminal UI, and the `norn --protocol jsonrpc` driven mode — assembles its agent through the *same* `AgentBuilder` path an embedder uses (`builder_from_cli` → `AgentBuilder`). There is no second assembly stack: a driver cannot configure an agent in a way a library consumer could not.

## Status

**v0.1.0.** Embedded as the agent runtime behind Meridian's AI assistant sessions. Multi-agent coordination, the tool suite, session persistence, the TUI, and provider abstraction are implemented and tested.

## Crates

| Crate | Approx. lines (src) | Approx. tests | Description |
|-------|--------------------:|--------------:|-------------|
| `norn` | ~150K | ~2,600 | Core runtime — agents, tools, sessions, providers, rules, config |
| `norn-cli` | ~16K | ~400 | The `norn` binary — print mode, JSON-RPC driven mode, subcommands |
| `norn-tui` | ~26K | ~690 | Terminal user interface |
| `norn-macros` | ~2K | ~60 | Derive macros (tool argument schemas, follow-ups) |

(Line and test counts are approximate, measured from the source tree.)

## Features

- **Multi-agent coordination** — Spawn child agents, fork context onto a different model, signal and wake agents. Agent registry with hierarchical paths, RAII spawn reservation, and a cooperative cancellation cascade.
- **Built-in tool suite** — Read, Write, Edit, ApplyPatch (all AST-aware, with read-before-write gating and file-length policy), Bash (with runtime risk classification), Search (content / files / fuzzy / AST modes), LSP, WebSearch, WebFetch, Task, Skill, ToolSearch, ActionLog, and the agent-coordination tools. Composite tools expose multiple modes/subcommands. Around twenty tools are registered by default.
- **Session persistence** — Append-only event store, action logs, surgical context editing (suppress / supersede / inject), session trees, and single-pass resume/fork of a persisted session.
- **Provider abstraction** — A `Provider` trait with a native OpenAI Responses API implementation (streaming, tool calling, structured output, reasoning effort, OAuth via `codex-login`), a generic OpenAI-compatible provider, and a Claude path via the `claude-runner` adapter.
- **Rules engine** — Path- and command-triggered contextual guidance with system-context-append, injection, and message delivery modes, and lifecycle tracking that re-injects only when a rule has left the active context.
- **Front-ends** — Interactive TUI, a print/headless CLI, and a JSON-RPC driven mode (`docs/design/norn-cli/DRIVEN-PROTOCOL.md`), all over the same library.

## Architecture

```
crates/norn/src/
├── agent/         — AgentBuilder (the single assembler), registry, forking,
│                    child policy, message router, goals, resume
├── loop/          — Agent step state machine (runner/: Gate → BuildRequest →
│                    CallProvider → Dispatch → ResolveStop), compaction, schema
├── provider/      — Provider trait, OpenAI Responses API, OpenAI-compatible, OAuth
├── session/       — Event store, action logs, context editing, session trees
├── rules/         — Rules engine (triggers, delivery modes, lifecycle)
├── tools/         — Tool implementations (bash, search, lsp, agent, web, task, …)
├── tool/          — Tool trait, lifecycle, registry, context, envelope
├── integration/   — Claude adapter, MCP client/server/proxy, Rhai, extensions, hooks
├── config/        — Settings loading, merge, permissions, validation
├── skill/         — Skill system (loadable SKILL.md prompt templates)
└── system_prompt/ — System prompt construction
```

## Building

```sh
cargo build --workspace
cargo run --bin norn -- --help      # CLI help
cargo run --bin norn -- mcp serve   # run Norn as an MCP server over stdio
```

Library examples live under `crates/norn/examples/` (`chat.rs`, `login.rs`, `smoke.rs`).

Lint and format gates (both must pass clean):

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

## Requirements

- Rust 1.94.0 via `rust-toolchain.toml` (edition 2024). This is the supported
  repository toolchain; no lower minimum Rust version is currently claimed.

## License

AGPL-3.0 (see `LICENSE`).
