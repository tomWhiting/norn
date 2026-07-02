# Norn — Checklist

## Crate Setup

- [ ] **C1** — norn crate exists as a workspace member with edition 2021
- [ ] **C2** — Cargo.toml declares unsafe_code = deny and pedantic clippy lints
- [ ] **C3** — src/lib.rs declares public modules: provider, loop, tool, tools, rules, agent, session, integration, error
- [ ] **C4** — error.rs defines NornError with thiserror, no anyhow in library code

## Provider Core

- [ ] **C5** — Provider trait defined with stream method returning typed ProviderEvent stream
- [ ] **C6** — ProviderEvent enum covers text_delta, thinking_delta, tool_call_delta, done, error
- [ ] **C7** — Usage struct tracks input tokens, output tokens, cache hits, and cost
- [ ] **C8** — OpenAI Responses API provider implements the Provider trait
- [ ] **C9** — OpenAI provider supports streaming via SSE
- [ ] **C10** — OpenAI provider supports tool calling with function definitions
- [ ] **C11** — OpenAI provider supports response_format with JSON schema for structured output
- [ ] **C12** — OpenAI provider supports reasoning effort control
- [ ] **C13** — OpenAI provider supports server-side web search via ToolSpec::WebSearch
- [ ] **C14** — OpenAI provider supports model selection across GPT-5.x family

## Agent Loop

- [ ] **C15** — Agent loop accepts provider, tools, context, instruction, and output schema
- [ ] **C16** — Agent loop executes prompt-tool cycle until model stops or schema-valid output is produced
- [ ] **C17** — Output schema validation rejects non-conforming output and feeds error back to model
- [ ] **C18** — Token-by-token streaming events emitted during LLM response
- [ ] **C19** — Per-event output schemas configurable for assistant message, spoken response, tool call envelope, stop output, question, handoff, review, progress
- [ ] **C20** — Inbound channels deliver messages at controlled tool boundaries without interrupting
- [ ] **C21** — Steer delivery mode injects after current tool batch before next LLM call
- [ ] **C22** — Follow-up delivery mode injects only when agent would otherwise stop
- [ ] **C23** — Token threshold detection warns when context approaches window limit
- [ ] **C24** — Soft handoff injects wrap-up guidance at configurable threshold
- [ ] **C25** — Repeated failure detection identifies looping on same error
- [ ] **C26** — Semantic quality signals detect hedging language and premature completion

## Tool Framework

- [ ] **C27** — Tool trait defines name, description, input_schema, pre_validate, execute, post_validate, on_success
- [ ] **C28** — Pre-validate phase supports both compile-time (baked-in) and runtime (profile-configured) checks
- [ ] **C29** — Post-validate phase supports both compile-time and runtime checks
- [ ] **C30** — On-success phase supports both compile-time and runtime follow-ups
- [ ] **C31** — Tool call envelope carries model-supplied args, runtime-supplied inputs, and open metadata field
- [ ] **C32** — Runtime-supplied tool arguments injected from profile/policy before execution
- [ ] **C33** — Effect-based parallel scheduling runs read-only tools concurrently, serializes write tools
- [ ] **C34** — Bash risk classification categorizes commands into five runtime-evaluated tiers
- [ ] **C35** — Dynamic tool availability changes available tools based on profile, workflow stage, or task state
- [ ] **C36** — ToolRegistry supports adding, removing, and querying tools at runtime

## Core Tools

- [ ] **C37** — Read tool reads files with line numbers, image support, and binary detection
- [ ] **C38** — Write tool enforces read-before-overwrite for existing files
- [ ] **C39** — Write tool validates AST via tree-sitter after writing
- [ ] **C40** — Write tool enforces configurable file length limits with path-specific overrides
- [ ] **C41** — Edit tool enforces read-before-edit
- [ ] **C42** — Edit tool validates AST via tree-sitter after editing
- [ ] **C43** — Edit tool reports diff output and blast radius via LSP
- [ ] **C44** — Search tool combines ripgrep, nucleo fuzzy search, glob filtering, and AST search
- [ ] **C45** — Bash tool streams output with progress detection
- [ ] **C46** — WebSearch tool delegates to OpenAI server-side search for Codex models
- [ ] **C47** — WebFetch tool fetches URLs and converts HTML to markdown
- [ ] **C48** — LSP tool provides hover, go-to-definition, find references, document symbols, and diagnostics
- [ ] **C49** — SpawnAgent tool creates a sub-agent with task, model, role, and optional forked context
- [ ] **C50** — Fork tool forks the current agent onto a different model and returns structured audit result
- [ ] **C51** — SendMessage, WaitAgent, and CloseAgent tools implement inter-agent coordination
- [ ] **C52** — Task tool supports create, list, update, and complete operations
- [ ] **C53** — Skill tool loads a SKILL.md prompt template into context
- [ ] **C54** — RunScript tool executes inline Rhai scripts with sandboxed host functions
- [ ] **C55** — ToolSearch tool performs BM25 search over the tool catalog

## Rules Engine

- [ ] **C56** — Rules parsed from files with YAML front matter specifying trigger conditions
- [ ] **C57** — Path glob triggers fire on file read/write matching configured patterns
- [ ] **C58** — Bash/tool command triggers fire on command/tool invocation matching configured patterns
- [ ] **C59** — Triggers configurable as before or after the matched action
- [ ] **C60** — System context append delivery mode adds rule to system prompt for the session
- [ ] **C61** — Context injection delivery mode delivers rule at next input boundary
- [ ] **C62** — Message delivery mode sends rule as a conversation message
- [ ] **C63** — Lifecycle tracking detects when a rule has been edited out of context and re-injects on next trigger
- [ ] **C64** — Rules with shell execution generate dynamic content at injection time

## Multi-Agent

- [ ] **C65** — Agent registry tracks active agents with hierarchical paths and statuses
- [ ] **C66** — No hardcoded concurrency limits on agent count
- [ ] **C67** — Two-phase spawn reservation with RAII cleanup on failure
- [ ] **C68** — Mailbox messaging supports trigger_turn flag for immediate vs deferred delivery
- [ ] **C69** — Sequence numbers on mailboxes enable efficient wait-for-any without polling
- [ ] **C70** — Fork inherits filtered parent context and returns structured audit result
- [ ] **C71** — RunMonitored tool delegates monitoring to a lightweight model and returns handle for queries
- [ ] **C72** — Goal tracking supports objectives, token budgets, time budgets, and continuation policies
- [ ] **C73** — Scheduling supports session-dispatched cron that re-launches sessions without keeping them alive

## Session and Context

- [ ] **C74** — Session events are append-only with IDs and parent IDs forming a tree
- [ ] **C75** — Suppress operation excludes events from prompt construction without deleting them
- [ ] **C76** — Summarize operation replaces event sequences with structured summaries
- [ ] **C77** — Inject operation adds external context into the event stream
- [ ] **C78** — Compact operation summarizes everything before a cut point
- [ ] **C79** — Prompt construction is a view over events, not a mutation

## Integration

- [ ] **C80** — Claude Runner integration routes steps via profile configuration and produces StepOutcome
- [ ] **C81** — Norn-wrapped Claude Code mode strips native tools and provides Norn tools via MCP
- [ ] **C82** — MCP client connects to external tool servers and registers tools dynamically
- [ ] **C83** — MCP server mode exposes all Norn tools via standard MCP protocol
- [ ] **C84** — Rhai builtins expose run_agent, spawn_agent, send_message, wait_agent, close_agent, fork_agent
- [ ] **C85** — Extension system integrates with Meridian extension protocol for shared out-of-process extensions
- [ ] **C86** — Diagnostics integration reports tool failures, schema violations, and policy violations at compiler grade
- [ ] **C87** — Session variables are declarative and scriptable with shell execution at prompt construction time
- [ ] **C88** — Lifecycle hooks support pre/post tool call, pre/post LLM call, and session events

## Diagnostics Wiring

- [ ] **C89** — LoopContext has an optional DiagnosticCollector field (Option<Arc<DiagnosticCollector>>)
- [ ] **C90** — Runner pushes NornDiagnostic into LoopContext::diagnostics on schema validation failure
- [ ] **C91** — Runner pushes NornDiagnostic into LoopContext::diagnostics on pre-validate block
- [ ] **C92** — Runner pushes NornDiagnostic into LoopContext::diagnostics on post-validate failure
- [ ] **C93** — File-modification tools count code lines via tokei (not naive non-blank line counting)
- [ ] **C94** — File length threshold is caller-configured with no hardcoded default — omitted threshold means no check
- [ ] **C95** — LengthLimit.default is Option<usize> where None disables the length check
- [ ] **C96** — PostValidateMode is configurable at runtime via ToolContext flags, not fixed per tool
- [ ] **C97** — Tool validation results include structured diagnostic JSON (code, line, severity, message, fix, do_not)
- [ ] **C98** — DiagnosticCollector is published on ToolContext typed infrastructure so RuntimePostValidateCheck implementations can push
