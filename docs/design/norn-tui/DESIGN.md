---
type: design
cluster: norn-tui
title: "Norn TUI: Terminal User Interface for the Agent Runtime"
---

# Norn TUI: Terminal User Interface for the Agent Runtime

## Intention

When this is done, working with Norn feels like pairing with a colleague in the terminal. You see what the agent is doing as it does it — streaming text, tool calls, thinking — and you see what its children are doing without losing your place. You scroll back through the conversation with your mouse wheel and copy text with your terminal's native selection. The interface is fast, honest about what it can and cannot render, and stays out of your way when you're reading.

The TUI is not a dashboard. It is a working surface. The agent's output streams into real terminal scrollback. The input area is always visible. When sub-agents spawn, their status appears; when they finish, the chrome disappears. The single-agent case has zero visual overhead.

## Problem

Norn's CLI has two modes: an interactive REPL (reedline) and a headless print mode. The REPL streams text and tool summaries linearly to stdout/stderr with no structure — no collapsible tool output, no syntax highlighting in code blocks, no multi-agent visibility, no visual separation between assistant text and tool execution. The print mode is headless and produces output for piping.

Every competing agent harness (Claude Code, Codex CLI, DeepSeek-TUI) has invested in terminal rendering: collapsible tool calls, syntax-highlighted code blocks, streaming with spinners, multi-agent views. Norn's REPL is functionally correct but visually bare.

The existing REPL also has architectural limitations: reedline's input model is single-line with history, which doesn't map well to the multi-line input and autocomplete surfaces an agent TUI needs. The streaming display (`print/display.rs`) writes raw text and ANSI dim sequences directly to stdout/stderr with no structured rendering.

## Solution

### D1: Library crate consumed by the existing binary

`norn-tui` is a library crate at `crates/norn-tui/`. The `norn` binary (in `norn-cli`) depends on it. When the binary detects an interactive terminal (both stdin and stdout are TTYs) and `--print` is not set, it launches the TUI instead of the reedline REPL. The TUI replaces the REPL — it is not an addition alongside it. Reedline may be dropped as a dependency.

### D2: Termina as the terminal backend

The TUI uses termina (from the Helix project) for terminal manipulation instead of crossterm. Termina exposes raw VT escape sequences, provides VT extension detection at setup (true colour, Kitty keyboard protocol, synchronized rendering), and has better SGR compression for styling. It is pure Rust with no C dependencies (CO1 compliant). crossterm is not used.

### D3: Scroll region architecture with DECSTBM

The terminal is split into two zones using DECSTBM (Set Top and Bottom Margins — a VT100 standard supported by every modern terminal):

- **Scroll region** (top): the conversation area. Text written here enters the terminal's native scrollback buffer. The user scrolls with mouse wheel, trackpad, or terminal shortcuts. Text selection and copy work natively.
- **Fixed panel** (bottom): the input area, agent status lines, and streaming indicator. Content here is under direct cursor-addressed control and never scrolls.

This architecture preserves native terminal scrollback — the user's terminal emulator owns the scroll buffer, not the TUI. Content in the scroll region is immutable once written; the TUI does not attempt to rewrite or modify scrollback content.

The fixed panel's height is dynamic. DECSTBM is reissued whenever the panel grows (multi-line input, agent status lines appearing, autocomplete popup) or shrinks (input submitted, agents completing, popup dismissed).

### D4: Dynamic fixed panel

The fixed panel contains, from top to bottom:

1. **Agent status lines** (0-5 rows) — one per active child agent. Absent in the single-agent case.
2. **Streaming indicator** (0-1 row) — shows `● generating...` with elapsed time while the model is producing output. Shows usage summary after completion. Absent when idle.
3. **Autocomplete popup** (0-8 rows) — appears above the input when triggered by `/`, `@`, or Tab. Dismissed on selection or Escape.
4. **Input area** (1-N rows) — single line by default. Grows upward as the user adds newlines via Shift+Enter or Alt+Enter.
5. **Status bar** (1 row) — model name, session info, token usage, key hints.

Each component's presence or absence is tracked. When the composition changes, the total panel height is recalculated and DECSTBM is reissued.

### D5: Token-by-token streaming into the scroll region

The TUI subscribes to the `broadcast::Sender<ProviderEvent>` that the agent loop already provides. Event handling:

- **TextDelta**: accumulated into a markdown parse buffer. Flushed to the scroll region at natural boundaries (sentence end, paragraph break, or when a ToolCallDelta arrives). Plain text flushes immediately; code fences and inline spans buffer until their closing marker arrives.
- **ThinkingDelta**: rendered with ANSI dim attribute directly into the scroll region. Collapsed by default; a keybinding toggles thinking visibility for future output.
- **ToolCallDelta**: accumulated per tool-call ID until Done or the next TextDelta. Then rendered through the per-tool renderer.
- **Done**: clears the streaming indicator, shows usage summary.
- **Error**: renders the error message in the scroll region with a distinct error style.

Content written to the scroll region is final. The TUI does not track scroll position or attempt to detect whether the user has scrolled back — the terminal's native scrollback handles this transparently.

### D6: Markdown rendering pipeline

Assistant text is rendered as formatted markdown before entering the scroll region. The pipeline:

1. **Accumulate** TextDelta chunks into a parse buffer.
2. **Parse** with pulldown-cmark (event-based streaming parser).
3. **Render** each markdown element:
   - **Bold** (`**text**`): ANSI bold attribute.
   - **Italic** (`*text*`): ANSI italic attribute (falls back to underline on terminals without italic support).
   - **Headers** (`# H1`, `## H2`): bold with size-appropriate indentation.
   - **Code spans** (`` `inline` ``): distinct foreground colour, no syntect pass.
   - **Fenced code blocks** (` ```lang `): syntax-highlighted via syntect using the language hint. Falls back to `find_syntax_by_first_line` for unlabeled blocks. Grammar set ships as syntect's compressed binary dump (~100 languages).
   - **Lists**: bullet/numbered with indentation.
   - **Horizontal rules**: `───` line spanning the terminal width.
   - **Links** (`[text](url)`): OSC 8 hyperlinks when the terminal supports them. Falls back to `text (url)` bracketed format.
4. **Write** the styled output to the scroll region.

**Partial markdown during streaming**: when a bold marker (`**`) arrives in one TextDelta but the closing marker arrives in a later delta, the parser buffers the incomplete span. The strategy mirrors code fence buffering: detect the opening marker, hold content until the closing marker arrives, then render the complete span. Plain text outside any open marker flushes immediately. This introduces a small rendering delay for inline spans (typically one or two TextDelta chunks) that is invisible to the user since the model emits styled text in bursts.

### D7: Per-tool renderers with tier classification

Each tool gets a renderer matched to its output complexity.

**Tier 1 — Rich (collapsible, multi-line body):**

| Tool | Header | Body |
|------|--------|------|
| Bash | `$ {command}` + spinner/exit code + duration | Streaming output with ANSI passthrough. Open while running, collapsed on completion. |
| Edit | `~ {path}` + AST status | Unified diff (red/green), blast-radius symbols. Collapsed. |
| ApplyPatch | `patch {path}` + hunk count | Diff hunks with +/- colouring. Collapsed. |
| Search | `? {query}` + result count | file:line:content with highlighted matches, grouped by file. Collapsed. |
| Read | `> {path}` + line range | Not shown by default (model reads, human rarely cares). Expandable. |

**Tier 2 — Compact (one or two lines, no body):**

| Tool | Format |
|------|--------|
| Write | `+ {path}` + line count + AST status |
| WebSearch | `web: {query}` + result count |
| WebFetch | `fetch: {url}` + content length |
| LSP | `lsp: {action}` + target |
| Task | `task: {action} "{title}"` + status |
| Skill | `skill: {name} loaded` |
| RunScript | `script: {name}` + output preview |
| ToolSearch | `search tools: {query}` + match count |

**Tier 3 — Minimal (inline status or fixed-panel only):**

| Tool | Rendering |
|------|-----------|
| SpawnAgent | Status line appears in fixed panel. Momentary `spawned` note in scroll region. |
| Fork | `fork → {model}` + task preview. Result on completion. |
| SignalAgent | `→ {agent_path}: {message_preview}` |
| WaitAgent | Not rendered. Wait is invisible; result appears on completion. |
| CloseAgent | `✕ {agent_path}` + cascade count |

All tool renderers share a header-line pattern: prefix character, tool-specific summary, spinner (while running) or duration (on completion). The header is both the live view and the collapsed view.

**Verbosity toggle**: Ctrl+O toggles the global default between collapsed and expanded for future tool calls. Content already in scrollback is immutable — the toggle only affects future rendering.

**Edit rollback case**: when AST validation fails and the edit is not committed (`kind: "edit_blocked_by_ast"`, `committed: false`), the header shows `✗ ~ {path}  AST BLOCKED (not committed)`. The body shows diagnostic errors, not the diff (showing a diff of uncommitted changes would be misleading). When `AllowBrokenAst` overrides the gate, the header shows `⚠ ~ {path}  COMMITTED (AST override: {source})` with the diff body and diagnostic warnings.

### D8: Input system

The input area lives in the fixed panel.

**Line editing**: single line by default. Shift+Enter inserts a newline (requires Kitty keyboard protocol for reliable Shift+Enter discrimination; falls back to Alt+Enter on standard terminals). The input area grows upward, reissuing DECSTBM. Enter submits. Escape clears. Ctrl+C on empty input exits.

**History**: Up/Down arrows cycle through previous inputs (when no autocomplete popup is open). History persists to `~/.norn/history.txt`.

**Autocomplete triggers**:

- `/` at column 0 — slash command and skill completion. Shows command name, source tag (`(builtin)` or `(profile)`), and description.
- `@` — file/directory path completion via nucleo fuzzy matching against the working tree.
- Tab — accept the current popup selection. If no popup is open and the cursor is mid-word, open completion for the nearest trigger.

**Autocomplete rendering**: a popup menu rendered inside the fixed panel, growing upward. The fixed panel temporarily expands (DECSTBM reissue) to fit the menu. Maximum 8 visible rows; additional candidates show a count indicator (`4 more...`). Up/Down navigate the menu. Typing narrows the candidates. Escape dismisses.

No inline ghost text. The popup is sufficient and avoids rendering complexity with multi-line input and ANSI styling.

### D9: Session event rendering

The TUI renders four categories of session events, each with distinct visual treatment:

- **Assistant message** (`SessionEvent::AssistantMessage`): primary display. Markdown-rendered via the D6 pipeline. When an output schema configures multiple fields, labeled sections render the primary field by default. Ctrl+E toggles whether secondary fields are shown for future structured messages.
- **Thinking content** (`ProviderEvent::ThinkingDelta`): rendered with ANSI dim attribute. Collapsed by default; Ctrl+E toggles thinking visibility for future output.
- **Tool calls** (`SessionEvent::ToolResult` + `ProviderEvent::ToolCallDelta`): rendered through the per-tool renderer (D7). Each tool type gets appropriate visual treatment.
- **User message** (`SessionEvent::UserMessage`): rendered with a distinct prefix to visually separate user input from assistant output.

When additional event types are defined in the EventSchemaSet, the TUI renders them using the labeled-section pattern: the primary field displays by default, secondary fields are accessible via Ctrl+E. The rendering is schema-driven — new event types with schemas render automatically without TUI code changes.

### D10: Agent tree visualisation in the fixed panel

When only the root agent is running, no agent status lines appear. When children spawn, status lines appear above the input area.

Each status line format:
```
{indent}{icon} {name}  {activity}  {tokens}  {elapsed}
```

Icons: `●` running, `◌` idle/waiting, `✓` done, `✗` failed, `⊙` spawning.

The tree uses 2-space indentation to show parent/child relationships. Maximum 5 visible agent lines. When the tree exceeds 5 lines, collapse heuristic:

1. Root agent: always visible.
2. Most recently spawned active agents: fill available slots.
3. Agents with status changes in the last 5 seconds: prioritised.
4. Oldest active agents: fill remaining slots.
5. Overflow: `⋯ N more active agents` summary line (focusable, Enter expands temporarily).

Completed/failed agents hold their status line for 3 seconds showing the terminal state, then the line is reclaimed.

**Navigation**: Tab cycles focus between agents. The focused agent's status line highlights. Enter on a focused agent switches the active tab to that agent.

### D11: Multi-agent tabs with EventStore replay

When multiple agents are running, tabs appear in the fixed panel. The active tab determines which agent's output streams into the scroll region.

Switching tabs replays the last N events (default 20) from the selected agent's EventStore into the scroll region. A separator marks each switch:

```
════════ switched to: researcher ════════
```

Replayed content renders through the same per-tool and markdown renderers as live content. Background agents accumulate events silently in their EventStores.

### D12: Terminal capability detection and progressive enhancement

Termina probes capabilities at startup. The capability set is stored in a `TerminalCaps` struct threaded through all rendering code.

**Hard requirements** (TUI refuses to start without these):

- DECSTBM scroll regions — VT100 standard, supported by every modern terminal.
- Basic ANSI styling (bold, dim, underline, SGR reset) — universal.
- 256-colour (8-bit ANSI) — the minimum colour mode.

When hard requirements are not met, the binary falls back to `--print` mode with an explanatory message.

**Progressive enhancement** (used when available):

| Capability | Detection | Enhancement | Fallback |
|------------|-----------|-------------|----------|
| True colour (24-bit RGB) | `$COLORTERM=truecolor` or termina DA1 | Full syntect colour palette, richer UI accents | 256-colour palette mapping |
| Kitty keyboard protocol | Termina capability probe | Reliable Shift+Enter discrimination | Alt+Enter for newlines |
| Synchronized rendering (DCS 2026) | Termina capability probe | Tear-free fixed-panel redraws | Cursor hide/show during redraws |
| OSC 8 hyperlinks | Terminal identification | File paths become clickable links | `text (url)` bracketed format |
| Italic attribute | Terminal identification | Italic for markdown `*emphasis*` | Underline fallback |

**Multiplexer compatibility**: tested on tmux. GNU screen and Zellij are expected to work with the caveat that native scrollback is captured by the multiplexer's pane buffer, not the outer terminal. Documented limitation, not a bug. Users experiencing unsatisfactory scroll behaviour in multiplexers should use `--print` mode.

## Goals

G1. The TUI replaces the reedline REPL as the default interactive mode of the `norn` binary. Print mode (`--print`) remains for headless/piped usage.

G2. Assistant text renders with markdown formatting (bold, italic, headers, lists, horizontal rules) and syntax-highlighted code blocks via syntect.

G3. Tool calls render through per-tool renderers with a three-tier classification (rich/compact/minimal). The verbosity toggle affects future output only (scrollback is immutable).

G4. The scroll region uses DECSTBM so that conversation content enters the terminal's native scrollback. Users scroll, select, and copy with their terminal's native mechanisms.

G5. The input area supports multi-line editing, history, and autocomplete for slash commands/skills, file paths, agent names, profile names, and session names.

G6. When sub-agents spawn, their status appears as lines in the fixed panel. The single-agent case has zero visual overhead.

G7. Tab switching between agents replays recent events from the target agent's EventStore into the scroll region.

G8. The TUI degrades gracefully across terminal capabilities, with DECSTBM + 256-colour as hard requirements and true colour, Kitty keyboard protocol, synchronized rendering, and OSC hyperlinks as progressive enhancements.

## Non-Goals

NG1. Extension rendering in the TUI. Extension visual components are a Meridian web-view concern, not a TUI concern.

NG2. In-scrollback editing. Content in the scroll region is immutable once written. No rewriting, no folding, no retroactive collapse/expand of tool calls already in scrollback.

NG3. Custom mouse interaction beyond the terminal's native scroll and select. No clickable buttons, no drag, no custom mouse regions.

NG4. Image or media rendering. The TUI is text-only. Image tool output (if any) shows a file path or URL, not the image.

NG5. Full alternate-screen takeover. The TUI does not use the alternate screen buffer. Content enters real scrollback.

NG6. tmux/screen integration beyond passive compatibility. The TUI does not attempt to control the multiplexer or communicate with it via escape sequences.

## Structure

```
crates/norn-tui/
├── Cargo.toml
├── src/
│   ├── lib.rs                    — public API: run_tui() entry point
│   ├── app/
│   │   ├── mod.rs                — pub mod + re-exports
│   │   ├── state.rs              — AppState: messages, scroll, input, agents, tabs
│   │   └── event_loop.rs         — main tokio::select! loop over terminal events + channels
│   ├── input/
│   │   ├── mod.rs                — pub mod + re-exports
│   │   ├── editor.rs             — multi-line input editor with history
│   │   ├── autocomplete.rs       — trigger detection, candidate generation, popup state
│   │   └── keybindings.rs        — key event → action mapping
│   ├── render/
│   │   ├── mod.rs                — pub mod + re-exports
│   │   ├── fixed_panel.rs        — fixed panel compositor (agent lines, indicator, popup, input, status)
│   │   ├── scroll_region.rs      — DECSTBM management, write-to-scroll helpers
│   │   ├── markdown.rs           — pulldown-cmark → styled terminal output with streaming buffering
│   │   ├── syntax.rs             — syntect integration: highlight code blocks, language detection
│   │   └── style.rs              — TerminalCaps-aware styling: colour mapping, attribute fallbacks
│   ├── tools/
│   │   ├── mod.rs                — pub mod + re-exports, ToolRenderer trait
│   │   ├── rich.rs               — Tier 1 renderers: Bash, Edit, ApplyPatch, Search, Read
│   │   ├── compact.rs            — Tier 2 renderers: Write, Web*, LSP, Task, Skill, Script, ToolSearch
│   │   └── minimal.rs            — Tier 3 renderers: SpawnAgent, Fork, Signal, Wait, Close
│   ├── agents/
│   │   ├── mod.rs                — pub mod + re-exports
│   │   ├── status_line.rs        — per-agent status line rendering and lifecycle
│   │   ├── tree.rs               — agent tree collapse heuristic, focus cycling
│   │   └── tabs.rs               — tab state, EventStore replay on switch
│   ├── events/
│   │   ├── mod.rs                — pub mod + re-exports
│   │   └── schema_render.rs      — per-EventType rendering: Question, Progress, Review, SpokenResponse
│   └── terminal/
│       ├── mod.rs                — pub mod + re-exports
│       ├── caps.rs               — TerminalCaps struct, detection logic via termina probes
│       └── setup.rs              — raw mode, scroll region init, cleanup on exit
└── tests/
```

## Current Inventory

### Existing norn-cli code consumed or replaced by norn-tui

| Component | File | Relationship |
|-----------|------|-------------|
| Mode detection | `cli/mode.rs` | Modified: TUI replaces Repl variant |
| REPL driver | `repl/driver.rs` | Replaced by `norn-tui::app::event_loop` |
| REPL prompt | `repl/prompt.rs` | Replaced by `norn-tui::input::editor` |
| REPL completion | `repl/completion.rs` | Replaced by `norn-tui::input::autocomplete` |
| REPL keybindings | `repl/keybindings.rs` | Replaced by `norn-tui::input::keybindings` |
| REPL history | `repl/history.rs` | Consumed: history file format preserved |
| Streaming display | `print/display.rs` | Replaced by `norn-tui::render` + `norn-tui::tools` |
| Runtime builder | `runtime/builder.rs` | Consumed: TUI calls `build_runtime()` unchanged |
| Slash commands | `commands/slash/` | Consumed: slash registry shared between TUI and print mode |
| Provider construction | `print/provider.rs` | Consumed: TUI calls `build_provider()` unchanged |
| Session persistence | `session/` | Consumed: TUI writes JSONL session files unchanged |

### Existing norn runtime infrastructure consumed by norn-tui

| Component | File | Usage |
|-----------|------|-------|
| ProviderEvent broadcast | `provider/events.rs` | TUI subscribes for streaming |
| EventStore | `session/store.rs` | Tab replay reads from per-agent stores |
| AgentRegistry | `agent/registry.rs` | Agent tree reads status, paths, models |
| AgentHandle | `tools/agent/handle.rs` | Status watch channel for live updates |
| EventSchemaSet | `loop/event_schemas.rs` | Per-event rendering decisions |
| InboundChannel | `loop/inbound.rs` | User replies to Question events routed as Steer messages |
| SlashCommandRegistry | `loop/commands.rs` | Autocomplete candidate source for / trigger |
| SkillSearchPaths | `tools/skill.rs` | Skill name completion |
| Scanner | `profile/loader.rs` | Profile name completion for @profile: |
| ToolRegistry | `tool/registry.rs` | Tool name and category metadata for renderers |

## Constraints

CO1. Pure Rust. No C dependencies. `unsafe_code = "deny"`.

CO2. Tokio as the async runtime. Terminal event stream integration via termina's async event reader within `tokio::select!`.

CO3. No file over 500 lines of code (excluding tests, comments, whitespace).

CO4. `mod.rs` contains only `pub mod` declarations and re-exports.

CO5. No `.unwrap()` or `.expect()` in library code. All error paths propagated via `thiserror`.

CO6. No hardcoded limits on agent count or tab count (per norn DESIGN.md D8 / CO3).

CO7. Scroll region content is immutable once written. The TUI does not rewrite terminal scrollback.

CO8. The fixed panel redraws are the only cursor-addressed rendering. Scroll region writes are append-only at the cursor position within the region's bounds.

CO9. DECSTBM + 256-colour are hard requirements. The TUI refuses to start without them and falls back to `--print` mode.

CO10. No Meridian dependencies at the crate level (per norn DESIGN.md CO10). The TUI is a standalone consumer of libnorn types.
