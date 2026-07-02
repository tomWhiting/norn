# Norn-Cli — Checklist

## Crate Setup

- [ ] **C1** — norn-cli crate exists as a workspace member with norn as a dependency
- [ ] **C2** — Binary target named 'norn' is declared in Cargo.toml
- [ ] **C3** — clap derive used for argument parsing with subcommand support
- [ ] **C4** — Cargo.toml declares reedline, clap, dirs, serde_json, tracing-subscriber as dependencies

## Mode Detection

- [ ] **C5** — Default mode is interactive REPL when both stdin and stdout are TTYs
- [ ] **C6** — -p / --print flag forces non-interactive print mode
- [ ] **C7** — Piped stdin (stdin not a TTY) auto-selects print mode
- [ ] **C8** — Piped stdout (stdout not a TTY) auto-selects print mode
- [ ] **C9** — Positional PROMPT arguments accepted in both modes

## Agent Configuration Flags

- [ ] **C10** — -m / --model flag overrides the profile model
- [ ] **C11** — --profile flag loads a Profile from file path or ~/.config/norn/profiles/ by name
- [ ] **C12** — -S / --system-prompt flag overrides profile system instructions
- [ ] **C13** — --append-system-prompt flag appends to profile system instructions
- [ ] **C14** — --allowed-tools flag sets tool allow-list (comma-separated, supports globs)
- [ ] **C15** — --disallowed-tools flag sets tool deny-list (comma-separated)
- [ ] **C16** — --reasoning-effort flag accepts low/medium/high and threads to LoopContext
- [ ] **C17** — --max-turns flag sets AgentLoopConfig::max_iterations
- [ ] **C18** — --timeout flag parses duration string and sets AgentLoopConfig::step_timeout
- [ ] **C19** — -C / --working-dir flag sets the working directory before agent execution
- [ ] **C20** — -c / --config flag accepts KEY=VALUE pairs and maps to AgentLoopConfig/ProviderConfig fields
- [ ] **C21** — --rules flag loads a RuleEngine from the specified YAML file
- [ ] **C22** — --variables flag sets session variables expanded as {{key}} in system instructions and tool descriptions
- [ ] **C23** — -e / --extension flag connects MCP extensions by URI (repeatable)

## Output Control Flags

- [ ] **C24** — -s / --output-schema flag accepts inline JSON (starts with {) or file path
- [ ] **C25** — --event-schema flag accepts TYPE=JSON|PATH pairs for per-event-type schemas (repeatable)
- [ ] **C26** — -f / --output-format flag accepts text, json, or stream-json
- [ ] **C27** — text format: final output to stdout, progress/tools to stderr
- [ ] **C28** — json format: single JSON envelope to stdout with output, usage, model, session_id, events, result
- [ ] **C29** — stream-json format: NDJSON events to stdout (text_delta, tool_call, tool_result, thinking_delta, completed)
- [ ] **C30** — -o / --output flag writes final output to file
- [ ] **C31** — -q / --quiet flag suppresses progress and tool output on stderr

## Session Control

- [ ] **C32** — -r / --resume flag resumes a session by ID or name (no arg = most recent)
- [ ] **C33** — --fork flag forks a session by ID or name (no arg = most recent)
- [ ] **C34** — --no-session flag disables session persistence for the current run
- [ ] **C35** — --session-name flag sets a human-readable name for the session

## Session Persistence

- [ ] **C36** — Sessions persisted as JSONL files at ~/.local/share/norn/sessions/{id}.jsonl
- [ ] **C37** — Session index maintained at ~/.local/share/norn/sessions/index.jsonl
- [ ] **C38** — Events appended to session file after each run_agent_step call
- [ ] **C39** — Session index updated atomically after each append
- [ ] **C40** — Resume reconstructs EventStore and message history from JSONL file
- [ ] **C41** — Fork copies source events into new session with Fork event appended
- [ ] **C42** — XDG_DATA_HOME respected for session storage location
- [ ] **C43** — macOS falls back to ~/Library/Application Support/norn/ when XDG vars unset

## Stdin Handling

- [ ] **C44** — Piped stdin detected via stdin.is_terminal() — no flag required
- [ ] **C45** — Piped stdin content read in full before execution
- [ ] **C46** — When both stdin content and positional PROMPT exist, stdin is prepended as context with <stdin> delimiters

## Interactive REPL

- [ ] **C47** — reedline line editor used for interactive input
- [ ] **C48** — Multi-line input supported via configurable key (Shift+Enter or Alt+Enter)
- [ ] **C49** — Command history persisted to ~/.local/share/norn/history.txt
- [ ] **C50** — Tab completion for slash commands
- [ ] **C51** — Tab completion for file paths
- [ ] **C52** — Prompt displays model name and session status
- [ ] **C53** — Ctrl+C interrupts current agent step
- [ ] **C54** — Ctrl+D exits the REPL
- [ ] **C55** — Multi-turn conversation maintains EventStore across turns
- [ ] **C56** — LoopContext dynamic sections cleared between turns, prompt commands re-evaluated

## Slash Commands

- [ ] **C57** — Slash commands processed via preprocess_input() in both REPL and print mode
- [ ] **C58** — /help lists available commands with descriptions
- [ ] **C59** — /tools lists available tools with names and descriptions
- [ ] **C60** — /model with no arg shows current model; with arg switches model
- [ ] **C61** — /schema with no arg shows current output schema; with arg sets it
- [ ] **C62** — /compact triggers real context compaction via ContextEdits::auto_compact_keeping_recent_turns
- [ ] **C63** — /clear resets the EventStore and message history
- [ ] **C64** — /session shows session ID, name, turn count, and cumulative token usage
- [ ] **C65** — /name sets a human-readable name for the current session
- [ ] **C66** — /variables lists active session variables and their values
- [ ] **C67** — /exit and /quit exit the REPL
- [ ] **C68** — Profile-registered slash commands loaded from SlashCommandRegistry

## Streaming Display

- [ ] **C69** — CLI subscribes to broadcast::Sender<ProviderEvent> for real-time streaming
- [ ] **C70** — Text deltas streamed to stdout character-by-character in REPL mode
- [ ] **C71** — Thinking deltas rendered dimmed on stderr
- [ ] **C72** — Tool call summary displayed when tool call is complete (tool name + key arguments)
- [ ] **C73** — Tool results displayed with head/tail truncation for long output
- [ ] **C74** — Schema validation feedback shown on stderr when schema attempt fails
- [ ] **C75** — Token usage summary (input/output tokens, elapsed time) shown after each step

## Profile Integration

- [ ] **C76** — Profile resolved from file path (contains / or .) or name from ~/.config/norn/profiles/
- [ ] **C77** — from_profile() used to build LoopContext and gated ToolRegistry
- [ ] **C78** — CLI flags override profile values (model, system prompt, tools, reasoning effort)
- [ ] **C79** — Per-event schemas from profile merged with --event-schema CLI flags
- [ ] **C80** — Profile prompt_commands threaded into LoopContext
- [ ] **C81** — Profile capabilities resolved into tool allow-list, instructions, and disallowed patterns

## Auth Subcommand

- [ ] **C82** — norn auth login triggers OAuth PKCE flow via norn::provider::auth::login()
- [ ] **C83** — norn auth logout clears credentials via norn::provider::auth::logout()
- [ ] **C84** — norn auth status reports login state, token expiry, and account ID without exposing tokens
- [ ] **C85** — --codex-home flag on auth login overrides the codex home directory

## Session Subcommands

- [ ] **C86** — norn session list shows sessions from current directory (--all for all)
- [ ] **C87** — norn session list supports --limit and --format (table or json)
- [ ] **C88** — norn session show displays session metadata and event summary
- [ ] **C89** — norn session resume enters REPL with resumed session
- [ ] **C90** — norn session fork creates new session from existing and enters REPL
- [ ] **C91** — norn session export serializes session to jsonl, json, or markdown
- [ ] **C92** — norn session remove deletes session file and index entry
- [ ] **C93** — Session ID accepts partial prefix (minimum 8 characters) for disambiguation

## MCP Subcommands

- [ ] **C94** — norn mcp serve starts Norn as MCP server on stdio with full tool registry
- [ ] **C95** — norn mcp connect tests connection to external MCP server and reports capabilities

## Doctor Subcommand

- [ ] **C96** — norn doctor checks OAuth status
- [ ] **C97** — norn doctor checks provider connectivity
- [ ] **C98** — norn doctor checks working directory permissions
- [ ] **C99** — norn doctor reports pass/fail with actionable remediation messages

## Shell Completions

- [ ] **C100** — norn completion generates scripts for bash, zsh, and fish via clap_complete

## Exit Codes

- [ ] **C101** — Exit code 0 on successful completion
- [ ] **C102** — Exit code 1 on agent error (provider failure, tool error, schema unreachable)
- [ ] **C103** — Exit code 2 on CLI argument error
- [ ] **C104** — Exit code 3 on authentication error

## Provider Construction

- [ ] **C105** — OpenAiProvider constructed with OAuth auth source by default
- [ ] **C106** — Base URL auto-detected from AuthSource (ChatGPT backend for OAuth, api.openai.com for API key)
- [ ] **C107** — Provider config respects -c overrides for base_url, max_retries, request_timeout, and provider_options
- [ ] **C108** — --provider flag selects backend: openai (default) or claude-runner (ClaudeRunnerAdapter)

## Runtime Wiring

- [ ] **C109** — SimpleTokenEstimator wired into LoopContext::token_estimator unconditionally
- [ ] **C110** — ContextEdits::new() wired into LoopContext::context_edits unconditionally
- [ ] **C111** — RetryPolicy constructed from -c retry_max and retry_base_delay overrides (default: no retry)
- [ ] **C112** — DiagnosticCollector constructed and drained at step completion
- [ ] **C113** — Diagnostics rendered on stderr in text mode, included in json envelope, emitted as events in stream-json
- [ ] **C114** — IterationMonitorConfig loaded from profile iteration_monitor section
- [ ] **C115** — -c retry_max maps to RetryPolicy::max_retries (loop-level), distinct from -c max_retries (HTTP-level ProviderConfig)
- [ ] **C116** — -c request_timeout maps to ProviderConfig::timeout (per-request HTTP timeout)
- [ ] **C117** — -c provider_options accepts inline JSON mapped to ProviderConfig::provider_options

## Diagnostics CLI Integration

- [ ] **C118** — CLI constructs DiagnosticCollector and sets it on LoopContext::diagnostics before run_agent_step
- [ ] **C119** — DiagnosticCollector published on ToolContext via ToolRegistry::set_context with insert_extension
- [ ] **C120** — Profile tool_config section parsed for per-tool validation thresholds (max_code_lines, glob overrides)
- [ ] **C121** — -c write.max_code_lines=N sets WriteTool length limit from CLI override
- [ ] **C122** — Profile tool_config for Write constructs LengthLimit with specified default and glob overrides
- [ ] **C123** — DiagnosticCollector drained after run_agent_step and rendered per output format (text=stderr, json=envelope, stream-json=events)
