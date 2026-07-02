# Norn-Runtime — Checklist

## Runtime Discriminant and Types

- [ ] **C1** — RuntimeKind enum with ClaudeRunner and Norn variants defined in types.rs
- [ ] **C2** — SessionConfig has an optional runtime field of type Option<RuntimeKind>
- [ ] **C3** — SessionHandle stores RuntimeKind and is queryable via the registry
- [ ] **C4** — AssistantError has NornBuildFailed and NornSessionFailed variants

## Session Lifecycle Branching

- [ ] **C5** — AssistantService::start_session branches on RuntimeKind after token acquisition
- [ ] **C6** — spawn_norn_loop method builds AgentBuilder, creates event channels, spawns async task
- [ ] **C7** — Existing spawn_session_loop (Claude Runner path) is unchanged and continues to work
- [ ] **C8** — Both runtime paths register a SessionHandle with the same SessionRegistry
- [ ] **C9** — Both runtime paths produce broadcast::Sender<AssistantEvent> for downstream consumers

## Norn Session Loop

- [ ] **C10** — run_norn_session_loop is an async function spawned via tokio::spawn (not spawn_blocking)
- [ ] **C11** — Norn session loop multiplexes AgentEvent broadcast, SessionCommand mpsc, idle timeout, and agent completion
- [ ] **C12** — SessionCommand::SendMessage delivers text via Norn InboundChannel
- [ ] **C13** — SessionCommand::Stop cancels via CancellationToken with graceful drain of remaining events
- [ ] **C14** — SessionCommand::Kill cancels via CancellationToken immediately
- [ ] **C15** — Idle timeout fires only after the agent has completed at least one turn
- [ ] **C16** — Session status transitions to Stopped when the Norn agent run completes
- [ ] **C17** — Completion watcher emits ServiceEvent::SessionCompleted or SessionFailed on the global EventBus

## Event Translation (Norn to AssistantEvent)

- [ ] **C18** — NornTranslateState defined in assistant/norn_translate.rs, parallel to TranslateState
- [ ] **C19** — ProviderEvent::TextDelta maps to AssistantEvent::TextDelta
- [ ] **C20** — ProviderEvent::ThinkingDelta maps to AssistantEvent::ThinkingDelta
- [ ] **C21** — ProviderEvent::ToolCallComplete maps to AssistantEvent::ToolUseStarted with complete input
- [ ] **C22** — ProviderEvent::ToolCallDelta maps to AssistantEvent::ToolInputDelta with resolved tool_call_id
- [ ] **C23** — ProviderEvent::ToolResult maps to AssistantEvent::ToolResult with is_error derived from output
- [ ] **C24** — ProviderEvent::TextComplete maps to AssistantEvent::AssistantMessage (replay-only snapshot)
- [ ] **C25** — ProviderEvent::ThinkingComplete maps to AssistantEvent::AssistantThinking (replay-only snapshot)
- [ ] **C26** — ProviderEvent::Done maps to AssistantEvent::Usage (is_turn_final: true) then TurnComplete
- [ ] **C27** — ProviderEvent::Error maps to AssistantEvent::Error with formatted message
- [ ] **C28** — NornTranslateState synthesizes SessionStarted from AgentBuilder config before first real event
- [ ] **C29** — Sub-agent events (agent_id != root) are wrapped in AssistantEvent::SubAgentEvent
- [ ] **C30** — Task/Agent tool calls emit SubAgentStarted, their results emit SubAgentCompleted
- [ ] **C31** — Per-turn state (tool IDs, sub-agents, block index map) is cleared on Done event

## Session Persistence

- [ ] **C32** — Norn sessions call AgentBuilder::session(store) with a JsonlSink-backed EventStore
- [ ] **C33** — Session JSONL files stored at {data_dir}/sessions/norn/{session_id}.jsonl
- [ ] **C34** — Session resume reads existing JSONL file and creates EventStore::with_sink_and_events
- [ ] **C35** — New sessions create EventStore::with_sink (empty events, append-only sink)
- [ ] **C36** — Session fork copies source JSONL to new path, then resumes from copy
- [ ] **C37** — PG persistence (persist_events) continues unchanged — subscribes to AssistantEvent broadcast
- [ ] **C38** — Redis streaming (stream_deltas_to_redis) continues unchanged
- [ ] **C39** — NornSessionStore type encapsulates server-side session file operations (create, resume, fork)
- [ ] **C40** — make_norn_step_runner calls .session() on AgentBuilder with workflow-scoped EventStore
- [ ] **C41** — Workflow session files stored at {data_dir}/sessions/workflow/{step_id}.jsonl

## Authentication

- [x] **C42** — Norn sessions authenticate via OAuth (codex-login), independent of TokenPool (Tom direction 2026-05-28)
- [x] **C43** — No pool token acquired for Norn sessions — provider uses AuthSource::OAuth
- [x] **C44** — Claude Runner uses pool tokens, Norn uses independent OAuth — clean separation

## Wake and Inbox Delivery

- [ ] **C45** — build_wake_prompt unchanged — produces text prompt for both runtimes
- [ ] **C46** — Wake prompt delivered as AgentBuilder::prompt() for Norn sessions
- [ ] **C47** — Mid-session DMs delivered via SessionCommand::SendMessage
- [ ] **C48** — WakeupService unchanged — calls start_session which branches on RuntimeKind
- [ ] **C49** — Atomic claim via try_claim_for_wakeup works with Norn sessions

## Wake Config Integration

- [ ] **C50** — wake_configs table has a runtime column (text, nullable, default null = server default)
- [ ] **C51** — session_config_from_wake_config reads runtime field and sets SessionConfig.runtime
- [ ] **C52** — Server config has a default_runtime setting (ClaudeRunner during transition, Norn after)

## Namespace Tools — Infrastructure

- [ ] **C53** — CallerContext (from TA-001) carries member_id, session_id, workspace_id as Arc extension on ToolContext
- [ ] **C54** — Namespace tools call service-layer functions directly (in-process, no HTTP)
- [ ] **C55** — register_meridian_tools adds enabled namespace tools to AgentBuilder via .tool()
- [ ] **C56** — Profile filtering via allowed_tools/disallowed_tools gates namespace availability
- [ ] **C57** — Namespace tool implementations live in crates/meridian-tools/src/
- [ ] **C58** — Each namespace tool uses meridian_ prefix in the tool registry name

## Namespace Tools — Critical

- [ ] **C59** — meridian_messaging tool implements 12 commands: send, inbox, read, search, mark_read, snooze, respond, retry, channel_send, channel_mentions, notify_check, notify_summary (aligned with TA-002)
- [ ] **C60** — meridian_member tool implements 7 commands: get, list, lookup, status_set, status_get, activity, set_profile (identity directory — hits MemberService)
- [ ] **C75** — meridian_workspace tool implements 4 commands: workspace_list, workspace_get, workspace_members, workspace_config (workspace environment — hits WorkspaceService)
- [ ] **C61** — meridian_source tool implements 25 commands: status, file_statuses, branches, log, blame, diff_file, diff_summary, tree_show, tree_add, tree_remove, tree_merge, tree_restack, tree_graft, worktree_list, worktree_create, worktree_remove, stage, unstage, stage_all, discard, commit, push, pull, fetch, remotes (aligned with TA-006)
- [ ] **C62** — meridian_branch tool implements 22 commands: branch_status, branch_submit, branch_land, branch_transition, branch_list, branch_show, branch_advance, stack_status, stack_submit, stack_sync, stack_land, stack_restack, pr_status, pr_create, pr_push, pr_sync, pr_approve, pr_request_changes, pr_comment, pr_merge, pr_close, pr_request_reviewers (aligned with TA-007)

## Namespace Tools — Important

- [ ] **C63** — meridian_workflow tool implements 6 workflow commands: workflow_list, workflow_show, workflow_run, workflow_status, workflow_history, workflow_cancel (aligned with TA-008)
- [ ] **C64** — meridian_workflow tool implements 8 scheduler commands: scheduler_list, scheduler_create, scheduler_get, scheduler_update, scheduler_delete, scheduler_enable, scheduler_disable, scheduler_run (redistributed from former admin tool, aligned with TA-008)
- [ ] **C65** — Former meridian_admin actions redistributed: workspace_list/get to meridian_workspace, channel_list/send to meridian_messaging, search to meridian_messaging, profile assignment to meridian_member (set_profile), scheduler to meridian_workflow

## WebSocket and Frontend Compatibility

- [ ] **C66** — ws/assistant.rs compiles and passes all existing tests without modification after all NR-* briefs land
- [ ] **C67** — ws/session_replay compiles and passes all existing tests without modification after all NR-* briefs land
- [ ] **C68** — WS resubscribe (try_resubscribe) handles Norn broadcast sender drop identically to Claude Runner drop

## Integration Verification

- [ ] **C69** — cargo clippy --workspace -- -D warnings passes clean
- [ ] **C70** — All existing assistant service tests pass without modification
- [ ] **C71** — Norn session loop tests verify complete event translation pipeline
- [ ] **C72** — Namespace tool tests verify all actions with mocked service layer
- [ ] **C73** — Session persistence tests verify create, resume, and fork flows
- [ ] **C74** — No file exceeds 500 lines of code (excluding tests, comments, whitespace)

## Mid-Session DM Delivery

- [ ] **C76** — wake_trigger delivers DMs to active Norn sessions via SessionCommand::SendMessage instead of returning WakeSkipped
- [ ] **C77** — Mid-session DM formatted via format_agent_dm_envelope — identical envelope shape to wake prompt delivery, notification marked delivered after successful send

## Notification Check at Tool Boundaries

- [ ] **C78** — MessagingService exposes get_pending_notifications and count_pending_notifications wrapper methods delegating to NotificationStore
- [ ] **C79** — NotificationChecker struct holds Arc<MessagingService> and member_id, constructed in spawn_norn_loop, published as ToolContext extension
- [ ] **C80** — Post-tool-batch notification check injects pending notifications as ChannelMessage with DeliveryMode::Steer via InboundSender at the runner drain point
- [ ] **C81** — Injected notifications are marked as delivered so they are not re-injected on subsequent tool batches

## Member Session Setup

- [ ] **C82** — norn-member.md profile exists with namespace tools (messaging, member, workspace, workflow), system_instructions for workspace member behavior, bash excluded
- [ ] **C83** — wake_config_session.rs idle_timeout_secs reads from wake_config or defaults to None instead of hardcoded Some(0)
- [ ] **C84** — Norn wake sessions with no explicit profile default to norn-member profile

## Frontend Runtime Awareness

- [ ] **C85** — Frontend session types include runtime_kind field, surfaced through useAssistantSession and useActiveAgentSessions hooks
- [ ] **C86** — No hardcoded 'claude' model fallback strings in AssistantPanel.tsx — runtime-aware fallback instead
- [ ] **C87** — Session toolbar displays a runtime badge (Norn or Claude Runner) in the ProfileRow with distinct visual treatment
