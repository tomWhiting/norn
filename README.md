# Norn

Headless AI agent runtime. The engine that powers autonomous agents with multi-agent coordination, comprehensive developer tools, and session persistence.

## What it is

Norn gives AI agents the ability to read and write files, run commands, search code, coordinate with other agents, and persist their work across sessions. It is designed to be embedded as a library — [Meridian](https://ablative.com.au) uses it as the runtime behind its AI assistant sessions.

## Status

**v0.1.0** — In active production use as an embedded library. Multi-agent coordination, 40+ tools, session persistence, TUI, and provider abstraction are implemented and tested.

## Crates

| Crate | Lines | Tests | Description |
|-------|-------|-------|-------------|
| `norn` | 165K | 1,923 | Core runtime — agents, tools, sessions, providers |
| `norn-cli` | 18K | 443 | CLI interface and dev tooling |
| `norn-tui` | 27K | 681 | Terminal user interface |
| `norn-macros` | 3K | 46 | Derive macros |

## Features

- **Multi-agent coordination** — Spawn child agents, fork context, message between agents. Agent registry with full lifecycle management.
- **40+ developer tools** — Bash, read, write, edit, search (AST + fuzzy), LSP integration, diagnostics, web fetch, tasks, skills.
- **Session persistence** — Action logs, context editing, session trees. Resume where you left off.
- **Provider abstraction** — OpenAI-compatible API, OAuth support. Multiple concurrent providers.
- **TUI** — Full terminal interface for interactive agent sessions.
- **Headless mode** — Run agents without a UI. Designed for embedding.

## Architecture

```
norn/
├── agent/      — Registry, forking, child policies, message routing
├── loop/       — Agent conversation loop, retry, compaction
├── provider/   — LLM provider trait, OpenAI-compatible, OAuth
├── session/    — Event model, persistence, action logs
├── skill/      — Skill system — loadable capabilities
├── tools/      — 40+ tool implementations
│   ├── bash/       — Shell command execution
│   ├── read/       — File reading
│   ├── write/      — File writing
│   ├── edit/       — File editing
│   ├── search/     — AST and fuzzy code search
│   ├── lsp/        — Language server protocol
│   ├── agent/      — Agent spawning, forking, signalling
│   ├── web/        — Web fetch and search
│   └── task/       — Task management
└── system_prompt/  — System prompt construction
```

## Requirements

- Rust 1.85+

## License

AGPL-3.0
