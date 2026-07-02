# Norn-Tui — User Stories

## Developer — Interactive Agent Conversation

**S1.** As a developer working with an agent, I want assistant text to render with bold, italic, headers, and syntax-highlighted code blocks so that the output is readable without switching to a separate viewer.

**S2.** As a developer reading agent output, I want to scroll back through the conversation using my terminal's native scrollback so that I can select and copy text normally.

**S3.** As a developer, I want tool calls to render as compact one-line summaries by default so that the conversation flow is not buried under verbose tool output.

**S4.** As a developer debugging agent behaviour, I want to toggle verbose tool output (Ctrl+O) so that I can see full tool details when I need them.

**S5.** As a developer, I want to see a streaming indicator while the model is generating so that I know the agent is working even when I have scrolled back.

**S6.** As a developer, I want Edit tool failures to show the diagnostic errors clearly and indicate that the file was not changed so that I understand why the edit was rejected.

## Developer — Writing Input

**S7.** As a developer composing a prompt, I want to type multi-line input with Shift+Enter for newlines and Enter to submit so that I can write detailed instructions without a separate editor.

**S8.** As a developer, I want slash command autocomplete when I type / so that I can discover and invoke commands without memorising them.

**S9.** As a developer, I want @-reference autocomplete for file paths so that I can reference files without typing full paths.

**S10.** As a developer, I want input history accessible via Up/Down arrows so that I can re-send or edit previous prompts.

**S11.** As a developer, I want Escape to clear my current input so that I can start over without backspacing through a long prompt.

## Developer — Monitoring Multi-Agent Work

**S12.** As a developer orchestrating agents, I want to see a live status line for each active child agent so that I know what they are doing without attaching to each one.

**S13.** As a developer with many agents running, I want the agent tree to collapse gracefully so that the fixed panel does not consume my entire terminal.

**S14.** As a developer, I want to switch between agent tabs so that I can review different agents' work in the same terminal session.

**S15.** As a developer switching tabs, I want recent context from the target agent's history to replay into the scroll region so that I have enough context to understand what the agent has been doing.

**S16.** As a developer, I want the single-agent case to have zero visual overhead so that status chrome only appears when it is useful.

## Developer — Interacting with Structured Output

**S17.** As a developer using event schemas, I want structured assistant messages to show the primary field by default with secondary fields toggleable so that the conversation is not cluttered with alternate renderings.

**S18.** As a developer receiving a Question event, I want to see numbered options and reply inline so that I can answer the agent's question without switching contexts.

**S19.** As a developer using spoken responses, I want the TTS-optimised text available as a secondary field so that I can review what would be spoken without enabling TTS.

## Developer — Working Across Terminal Environments

**S20.** As a developer using a basic terminal, I want the TUI to degrade gracefully to 256-colour rendering so that the interface is functional without true colour support.

**S21.** As a developer in tmux, I want the TUI to work correctly within a tmux pane so that I can use my normal terminal multiplexer workflow.

**S22.** As a developer whose terminal lacks the Kitty keyboard protocol, I want Alt+Enter to work as a newline alternative so that multi-line input is still available.

**S23.** As a developer using a terminal with OSC 8 support, I want file paths in tool output to be clickable links so that I can open files directly.

## AI Agent — Rendering Output

**S24.** As an AI agent producing markdown, I want my code blocks to render with syntax highlighting so that the human can read code samples comfortably.

**S25.** As an AI agent calling tools, I want my tool calls to render with appropriate visual treatment per tool type so that the human can distinguish different kinds of work at a glance.

**S26.** As an AI agent spawning sub-agents, I want the sub-agent's status to appear in the human's interface so that the human can see delegation happening.

## Workflow Operator — Headless and Print Mode

**S27.** As a workflow operator piping agent output, I want the binary to auto-detect non-TTY and use print mode so that my scripts continue to work after the TUI is introduced.

**S28.** As a workflow operator, I want --print to force headless mode even on a TTY so that I can capture structured output from an interactive terminal.
