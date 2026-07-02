# Norn-Tui — Checklist

## Crate Setup

- [ ] **C1** — norn-tui crate exists as a workspace member at crates/norn-tui/
- [ ] **C2** — Cargo.toml declares termina, pulldown-cmark, syntect, unicode-width, tokio dependencies
- [ ] **C3** — src/lib.rs declares public modules: app, input, render, tools, agents, events, terminal
- [ ] **C4** — norn-cli depends on norn-tui as a path dependency
- [ ] **C5** — norn-cli mode detection dispatches to norn-tui when stdin+stdout are TTYs and --print is not set

## Terminal Setup and Capabilities

- [ ] **C6** — TerminalCaps struct detects and stores: true_colour, kitty_keyboard, synchronized_rendering, osc_hyperlinks, italic_support
- [ ] **C7** — Terminal enters raw mode on startup and restores on exit (including panic cleanup)
- [ ] **C8** — DECSTBM scroll region is set on startup, splitting the terminal into scroll region and fixed panel
- [ ] **C9** — DECSTBM is reissued whenever the fixed panel height changes
- [ ] **C10** — TUI refuses to start if DECSTBM or 256-colour are not available, printing an explanatory message and falling back to --print mode

## Scroll Region and Streaming

- [ ] **C11** — TextDelta events write styled markdown output into the scroll region
- [ ] **C12** — ThinkingDelta events render with ANSI dim attribute in the scroll region
- [ ] **C13** — ToolCallDelta events accumulate per tool-call ID and render through the per-tool renderer on completion
- [ ] **C14** — Done events clear the streaming indicator and show usage summary in the fixed panel
- [ ] **C15** — Error events render with a distinct error style in the scroll region
- [ ] **C16** — Streaming indicator in the fixed panel shows generating status with elapsed time while the model is producing output

## Markdown Rendering

- [ ] **C17** — Bold text (**bold**) renders with ANSI bold attribute
- [ ] **C18** — Italic text (*italic*) renders with ANSI italic attribute, falling back to underline
- [ ] **C19** — Headers (# H1, ## H2) render with bold and size-appropriate formatting
- [ ] **C20** — Inline code (`code`) renders with distinct foreground colour
- [ ] **C21** — Fenced code blocks render with syntect syntax highlighting using the language hint
- [ ] **C22** — Unlabeled code blocks fall back to syntect find_syntax_by_first_line
- [ ] **C23** — Lists (bullet and numbered) render with appropriate indentation
- [ ] **C24** — Horizontal rules render as a line spanning the terminal width
- [ ] **C25** — Links render as OSC 8 hyperlinks when supported, bracketed text otherwise
- [ ] **C26** — Incomplete inline spans (bold, italic) buffer until the closing marker arrives before rendering
- [ ] **C27** — Code fences buffer from opening to closing marker, then render the complete block with highlighting

## Per-Tool Renderers

- [ ] **C28** — ToolRenderer trait defines the interface for all tool renderers: header_line() and body()
- [ ] **C29** — Bash renderer streams output with ANSI passthrough, shows exit code and duration on completion
- [ ] **C30** — Edit renderer shows unified diff with red/green colouring and blast-radius symbols on success
- [ ] **C31** — Edit renderer shows AST BLOCKED header with diagnostic errors (not diff) when edit is rolled back
- [ ] **C32** — Edit renderer shows COMMITTED with override attribution when AllowBrokenAst is active
- [ ] **C33** — ApplyPatch renderer shows diff hunks with +/- colouring
- [ ] **C34** — Search renderer shows file:line:content with highlighted matches grouped by file
- [ ] **C35** — Read renderer shows file path and line range, body hidden by default
- [ ] **C36** — Write renderer shows file path, line count, and AST status on one line
- [ ] **C37** — WebSearch renderer shows query and result count on one line
- [ ] **C38** — WebFetch renderer shows URL and content length on one line
- [ ] **C39** — SpawnAgent renderer creates a status line in the fixed panel, not the scroll region
- [ ] **C40** — WaitAgent is not rendered as a tool call — the wait is invisible to the user
- [ ] **C41** — Ctrl+O toggles global verbosity for future tool calls without affecting scrollback content

## Input System

- [ ] **C42** — Enter submits input to the agent loop
- [ ] **C43** — Shift+Enter (Kitty keyboard) or Alt+Enter (fallback) inserts a newline and expands the input area
- [ ] **C44** — Input area grows upward, reissuing DECSTBM to shrink the scroll region
- [ ] **C45** — Escape clears the current input
- [ ] **C46** — Ctrl+C on empty input exits the TUI
- [ ] **C47** — Up/Down arrows cycle through input history when no autocomplete popup is open
- [ ] **C48** — Input history persists to ~/.norn/history.txt

## Autocomplete

- [ ] **C49** — / at column 0 triggers slash command and skill completion
- [ ] **C50** — Slash command popup shows name, source tag (builtin/profile), and description
- [ ] **C51** — @ triggers file/directory path completion via nucleo fuzzy matching (nucleo-matcher dependency comes through the norn crate)
- [ ] **C52** — Autocomplete popup renders inside the fixed panel, growing upward with DECSTBM reissue
- [ ] **C53** — Autocomplete popup shows at most 8 rows with a count indicator for additional candidates
- [ ] **C54** — Tab accepts the current popup selection
- [ ] **C55** — Escape dismisses the autocomplete popup

## Agent Tree and Multi-Agent Tabs

- [ ] **C56** — No agent status lines appear in the single-agent case
- [ ] **C57** — Agent status lines show indent, icon, name, activity, token count, and elapsed time
- [ ] **C58** — Agent status icons distinguish running, idle, done, failed, and spawning states
- [ ] **C59** — Agent tree collapses to at most 5 visible lines with overflow summary
- [ ] **C60** — Root agent is always visible when children exist
- [ ] **C61** — Completed/failed agents hold their status line for 3 seconds before reclaim
- [ ] **C62** — Tab cycles focus between agents in the fixed panel
- [ ] **C63** — Enter on a focused agent switches the active tab to that agent
- [ ] **C64** — Tab switch replays the last N events from the target agent's EventStore into the scroll region
- [ ] **C65** — A separator line marks each tab switch in the scroll region

## Session Event Rendering

- [ ] **C66** — Assistant messages render as markdown through the D6 rendering pipeline
- [ ] **C67** — Thinking content renders with ANSI dim attribute, collapsed by default
- [ ] **C68** — Ctrl+E toggles thinking visibility and secondary schema fields for future output
- [ ] **C69** — Tool calls render through per-tool renderers (D7) with appropriate visual treatment per tool type
- [ ] **C70** — User messages render with a distinct prefix separating them from assistant output
- [ ] **C71** — Structured assistant messages with multiple schema fields render the primary field by default with secondary fields toggleable
- [ ] **C72** — Secondary schema fields render with labeled separators between sections
- [ ] **C73** — New event types with EventSchemaSet schemas render automatically via the labeled-section pattern

## Fixed Panel Compositor

- [ ] **C74** — Fixed panel height is the sum of its active components (agent lines + indicator + popup + input + status bar)
- [ ] **C75** — Fixed panel redraws use synchronized rendering when available, cursor hide/show otherwise
- [ ] **C76** — Status bar shows model name, session info, token usage, and key hints
- [ ] **C77** — Fixed panel redraws do not affect scroll region content

## Progressive Enhancement

- [ ] **C78** — True colour detected via COLORTERM or termina DA1, falls back to 256-colour palette mapping
- [ ] **C79** — Kitty keyboard protocol used for Shift+Enter when available, Alt+Enter otherwise
- [ ] **C80** — Synchronized rendering wraps fixed-panel redraws when available
- [ ] **C81** — OSC 8 hyperlinks used for file paths when available, bracketed text otherwise
- [ ] **C82** — Italic attribute used for markdown emphasis when available, underline otherwise
