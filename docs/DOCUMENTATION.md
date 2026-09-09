# Norn documentation

Updated 8 September 2026, Melbourne time. Start with the [README](../README.md) for the current preview, installation from `main`, sign-in, model defaults, and runnable examples.

## Operating guides

| Topic | Guide |
| --- | --- |
| MCP server definitions and Claude Code Channels-compatible push | [MCP launch and Channels](MCP-LAUNCH.md): inline JSON, files, saved policies, wake behaviour, flags, and startup errors. |
| Terminal layout, composer, and shortcuts | [TUI preferences](TUI-PREFERENCES.md): send keys, editable bindings, settings files, save scope, and conflicts. |
| Workflow and application integration | [Driven mode](DRIVEN-MODE-GUIDE.md) and its [JSON-RPC contract](design/norn-cli/DRIVEN-PROTOCOL.md). |
| Provider setup | [Provider backends](provider-backends.md). |
| Image and other visual content | [Visual content](VISUAL-CONTENT.md). |
| Changes and verification | [Release notes](release-notes/UNRELEASED.md) and the [preview.8 installation record](design/norn-retained-tui/briefs/NUI-005.md). |

## Command help

Start the interactive terminal UI with `norn`; use `norn -p "your prompt"` for a one-shot run. Use `norn --resume SESSION_ID` to resume saved context. Resuming a saved session is distinct from attaching to a running background agent; live attachment remains separate work.

```sh
norn --help
norn auth --help
norn session --help
norn mcp --help
norn completion --help
```

Inside the TUI, `/help` lists slash commands and `/view help` lists frontend controls. Tool selection at startup uses `--allowed-tools` and `--disallowed-tools`; MCP server setup uses `--mcp-config`. The default OpenAI route uses `norn auth login` for ChatGPT OAuth rather than requiring an API key.

## Planned work

[Ordered September 8 work plan](planning/NEXT-WORK-20260908.md) consolidates the compaction and resumed-history reports, revised tool/channel presentation, action-log and memory proposals, voice hooks, and the earlier product backlog. It is planning only; implementation and verification are deferred until resources are available. Its [carry-forward inventory](planning/CARRY-FORWARD-20260908.json) preserves the earlier programme IDs and historical source status.

## Runtime and design

Norn supplies the agent session, provider access, tool execution, coordination, and recorded history. CLI, TUI, and driven callers construct agents through the same library assembly path; running Norn does not require a running Aion instance.

The [library examples](../crates/norn/examples) demonstrate embedding. The [design directory](design) contains architecture decisions and implementation briefs. Those documents include planned and historical work; consult each brief's status and verification record before treating a feature as installed. [CLAUDE.md](../CLAUDE.md) defines contribution and code-quality rules.
