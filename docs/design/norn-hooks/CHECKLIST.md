# Norn-Hooks — Checklist

## Module Promotion and HookOutcome Extension

- [ ] **C1** — integration/hooks.rs promoted to integration/hooks/ folder module with mod.rs and traits.rs.
- [ ] **C2** — All existing hook trait tests pass unchanged after module promotion.
- [ ] **C3** — HookOutcome has three variants: Proceed, Block { reason }, Modify { updated_input }.
- [ ] **C4** — HookOutcome::Modify returned only from PreToolHook. Other pre-hooks return Proceed or Block only.
- [ ] **C5** — tool_dispatch.rs handles Modify by replacing model_args and passing serialized modified args to execute_single_tool.
- [ ] **C6** — pre_validate receives modified args when HookOutcome::Modify is returned by a pre-tool hook.
- [ ] **C7** — HookType enum in error.rs has variants: PreTool, PreLlm, UserPrompt, Stop, SubagentStop, PreCompaction.
- [ ] **C8** — Hook enum expanded with variants for all new hook traits (UserPrompt, Stop, Subagent, SessionLifecycle, Compaction, PostToolFailure).

## New Hook Traits

- [ ] **C9** — UserPromptHook trait with async on_user_prompt method that returns HookOutcome (Proceed or Block).
- [ ] **C10** — StopHook trait with async on_stop method that returns HookOutcome (Proceed or Block).
- [ ] **C11** — SubagentHook trait with async on_subagent_start (observational) and on_subagent_stop (returns HookOutcome) methods.
- [ ] **C12** — SessionLifecycleHook trait with async on_session_start and on_session_end methods (observational, no return).
- [ ] **C13** — CompactionHook trait with async before_compaction method that returns HookOutcome (Proceed or Block).
- [ ] **C14** — PostToolFailureHook trait with async after_tool_failure method (observational, no return).
- [ ] **C15** — All new traits registered on HookRegistry via Hook enum variants with per-category dispatch methods.

## Config Schema

- [ ] **C16** — HookEventType closed enum with 13 snake_case names: pre_tool, post_tool, post_tool_failure, pre_llm, post_llm, session_event, user_prompt, stop, subagent_start, subagent_stop, session_start, session_end, pre_compaction.
- [ ] **C17** — HookGroupConfig deserializes from JSON with optional matcher field and required hooks array.
- [ ] **C18** — HookCommandConfig has required type (command), required command string, and required timeout (seconds).
- [ ] **C19** — Timeout field on HookCommandConfig is required with no hardcoded default.

## Settings Loading

- [ ] **C20** — Hooks loaded from ~/.norn/settings.json as user-global scope.
- [ ] **C21** — Hooks loaded from .norn/settings.json as project scope.
- [ ] **C22** — Hooks loaded from .norn/settings.local.json as local project override scope.
- [ ] **C23** — Hook groups concatenated across tiers in priority order (user-global first, local-project last) per event type.
- [ ] **C24** — Settings captured at startup; runtime modifications have no effect on current session.
- [ ] **C25** — load_hooks_from_settings called in norn-cli builder.rs, registers ShellCommandHook instances on HookRegistry.

## Shell Command Execution

- [ ] **C26** — ShellCommandHook spawns sh -c <command> with HookInput JSON piped to stdin.
- [ ] **C27** — ShellCommandHook enforces configured timeout via tokio::time::timeout; kills child on expiry.
- [ ] **C28** — Exit code 0: stdout parsed as JSON HookOutput and mapped to HookOutcome.
- [ ] **C29** — Exit code 2: HookOutcome::Block returned with stderr as reason.
- [ ] **C30** — Other exit codes: warning logged, HookOutcome::Proceed returned.
- [ ] **C31** — Empty or non-JSON stdout on exit 0: HookOutcome::Proceed returned (graceful degradation).
- [ ] **C32** — ShellCommandHook checks matcher before spawning; returns Proceed immediately on no match.

## Hook Input and Output Protocol

- [ ] **C33** — HookInput contains common fields: session_id, cwd, hook_event_name, agent_id, profile_name.
- [ ] **C34** — Tool-event HookInput includes tool_name, tool_input, tool_call_id.
- [ ] **C35** — Post-tool HookInput includes tool_output, tool_duration_ms, tool_is_error.
- [ ] **C36** — LLM-event HookInput includes model and message_count.
- [ ] **C37** — Stop HookInput includes final_text.
- [ ] **C38** — Subagent HookInput includes subagent_id and subagent_type.
- [ ] **C39** — HookOutput for pre_tool supports decision field with values proceed, block, modify.
- [ ] **C40** — HookOutput for pre_tool with decision=modify requires updated_input field.
- [ ] **C41** — HookOutput for other blocking events supports decision field with values proceed, block.
- [ ] **C42** — Environment variables NORN_PROJECT_DIR, NORN_SESSION_ID, NORN_AGENT_ID, NORN_PROFILE, NORN_HOOK_EVENT set on shell hook child process.

## Session Event Dispatch

- [ ] **C43** — Shell session event hooks fire via tokio::spawn with timeout (fire-and-forget).
- [ ] **C44** — Trait-based SessionEventHook implementations dispatched synchronously in registration order.
- [ ] **C45** — Non-reentrant runtime guard on HookRegistry prevents recursive session event hook dispatch.

## Matchers

- [ ] **C46** — HookMatcher compiles regex pattern at settings load time.
- [ ] **C47** — Invalid regex pattern rejected as config error at startup.
- [ ] **C48** — Tool events (pre_tool, post_tool, post_tool_failure) match against tool name.
- [ ] **C49** — LLM events (pre_llm, post_llm) match against model name.
- [ ] **C50** — Subagent events (subagent_start, subagent_stop) match against profile name or agent type.
- [ ] **C51** — Session events match against event variant name.
- [ ] **C52** — Absent matcher, empty string, or * matches all events of that type.
- [ ] **C53** — Events without matcher support (user_prompt, stop, session_start, session_end, pre_compaction) always fire when registered.

## Hook Wiring in Agent Loop

- [ ] **C54** — UserPromptHook dispatched at the entry of run_agent_step before initial message construction.
- [ ] **C55** — StopHook dispatched after the final iteration before AgentStepResult is returned.
- [ ] **C56** — SubagentHook on_subagent_start dispatched in spawn.rs launch function.
- [ ] **C57** — SubagentHook on_subagent_stop dispatched in spawn.rs finish function.
- [ ] **C58** — CompactionHook dispatched in compaction.rs before compaction event is appended.
- [ ] **C59** — PostToolFailureHook dispatched in tool_dispatch.rs when tool output contains an error.
- [ ] **C60** — SessionLifecycleHook on_session_start dispatched in norn-cli runtime at session construction.
- [ ] **C61** — SessionLifecycleHook on_session_end dispatched in norn-cli runtime at session teardown.
