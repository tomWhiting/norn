# Visual content plan

Placement suggestions for screenshots, terminal recordings, and video
walkthroughs that would strengthen Norn's documentation.

## README

| Location | Content | Format |
|---|---|---|
| After "What it is" | Terminal recording: `cargo run --bin norn -- --help` showing the CLI surface | asciinema or GIF, ~15s |
| After "Architecture" | Diagram: the AgentBuilder assembly path from embedder/CLI/TUI through to the agent loop | SVG |
| After "Features" | Terminal recording: a simple agent session — prompt, tool calls, response — in print mode | asciinema or GIF, ~30s |

## TUI

| Location | Content | Format |
|---|---|---|
| README or a dedicated TUI section | Screenshot of the TUI with an active agent session (tool output visible) | PNG, light and dark variants |
| README or a dedicated TUI section | Terminal recording: launching the TUI, submitting a prompt, watching tool execution | asciinema or GIF, ~30s |

## Embedding / library usage

| Location | Content | Format |
|---|---|---|
| A future embedding guide | Code walkthrough screenshot: `chat.rs` example with key lines annotated | PNG |
| A future embedding guide | Diagram: embedder constructs AgentBuilder, calls build(), runs the agent — showing the single assembly path | SVG |

## Multi-agent coordination

| Location | Content | Format |
|---|---|---|
| A future multi-agent guide | Diagram: agent registry tree with hierarchical paths, parent/child relationships, and the cancellation cascade | SVG |
| A future multi-agent guide | Terminal recording: spawning a child agent, watching it execute and report back | asciinema or GIF, ~30s |

## JSON-RPC driven mode

| Location | Content | Format |
|---|---|---|
| `docs/design/norn-cli/DRIVEN-PROTOCOL.md` | Sequence diagram: driver ↔ norn process JSON-RPC message flow (init, prompt, tool approval, result) | SVG |

## Session persistence

| Location | Content | Format |
|---|---|---|
| A future persistence guide | Diagram: append-only event store, action logs, session trees, and the resume/fork flow | SVG |
| A future persistence guide | Terminal recording: running a session, stopping, resuming from the persisted state | asciinema or GIF, ~30s |

## Video walkthrough ideas

| Topic | Duration | Audience |
|---|---|---|
| "Zero to agent" — install Rust, build Norn, run the chat example | 5 min | Developers new to Norn |
| "Embedding Norn" — using AgentBuilder in your own Rust project | 8 min | Library consumers |
| "The tool suite" — demonstrating Read, Write, Edit, Bash, Search in a live session | 10 min | Developers evaluating Norn's capabilities |
| "Multi-agent coordination" — spawning child agents, forking context, cooperative cancellation | 8 min | Developers building agent orchestration |
| "Norn vs. the field" — architectural comparison with other agent runtimes | 5 min | Technical decision-makers |

## Tools

- **Terminal recordings**: [asciinema](https://asciinema.org) (renders as text, accessible) or [VHS](https://github.com/charmbracelet/vhs) (GIF/MP4 from a script)
- **Architecture diagrams**: hand-drawn SVG or [Excalidraw](https://excalidraw.com) for the sketch aesthetic
- **Screenshots**: macOS with a clean terminal (ghostty or iTerm2, dark theme matching the Ablative brand)
