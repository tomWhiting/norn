# Norn-Agents — Checklist

## Profile System

- [ ] **C1** — Profile parser accepts markdown files with YAML frontmatter and extracts structured fields plus markdown body as system prompt.
- [ ] **C2** — Profile frontmatter supports name, description, model, tools (list or comma-separated), disallowedTools, reasoning_effort, reasoning_summary, and capabilities fields.
- [ ] **C3** — Profiles resolve from ~/.norn/profiles/ as .md files.
- [ ] **C4** — Two-tier scanning: workspace-level ({cwd}/.norn/profiles/) checked first, user-level (~/.norn/profiles/) as fallback. Workspace wins on name collision.
- [ ] **C5** — Meridian profiles at {cwd}/.meridian/profiles/ are checked as an additional scan tier.
- [ ] **C6** — Profile resolution is available from libnorn (not CLI-only) so SpawnAgentTool can load profiles at runtime.
- [ ] **C7** — norn-cli profile_loader.rs delegates to libnorn's profile loader instead of its own TOML/JSON parser.
- [ ] **C8** — paths.rs profiles_dir() returns ~/.norn/profiles/ not ~/.norn/config/profiles/.
- [ ] **C9** — Capability parser accepts markdown files with YAML frontmatter and extracts tools, disallowedTools, and prompt_fragment.
- [ ] **C10** — Capability resolution merges capabilities into a profile: tools union, disallowedTools append, prompt_fragment appends to system prompt.

## Persistent Task Store

- [ ] **C11** — DiskTaskStore persists tasks to ~/.norn/tasks/{group-slug}/ as individual {task-id}.json files. Task groups are session-agnostic.
- [ ] **C12** — DiskTaskStore implements the existing TaskStore trait with all five operations (create, get, list, update, complete) plus create_group, list_groups.
- [ ] **C13** — TaskEntry has parent_task_id: Option<String> for hierarchical task trees.
- [ ] **C14** — TaskEntry has assigned_agent: Option<String> recording which agent (by registry path) is assigned to the task.
- [ ] **C15** — create_subtask creates a task with parent_task_id set, forming a tree.
- [ ] **C16** — children(parent_id) returns direct child tasks of a parent.
- [ ] **C17** — ancestors(task_id) walks the parent chain to the root task.
- [ ] **C18** — Status roll-up: parent effective status is Blocked if any child Blocked, InProgress if any child InProgress, Completed only when all children Completed.
- [ ] **C19** — claim(task_id, agent_path) atomically sets assigned_agent only if currently unassigned.
- [ ] **C20** — SharedTaskStore installed on ToolContext during build_runtime.
- [ ] **C21** — InMemoryTaskStore remains available for testing and ephemeral sessions.

## ToolContext Extension Wiring

- [ ] **C22** — AgentToolInfra installed on ToolContext during build_runtime with AgentRegistry, Mailbox, Provider, EventStore, root agent UUID, and ToolRegistry.
- [ ] **C23** — SharedToolCatalog installed on ToolContext during build_runtime, built from the registry's tool definitions.
- [ ] **C24** — All five agent tools (spawn_agent, fork, send_message, wait_agent, close_agent) return successful results when invoked, not extension-missing errors.
- [ ] **C25** — task tool returns successful results when invoked, not extension-missing errors.
- [ ] **C26** — tool_search tool returns successful results when invoked, not extension-missing errors.

## Async Spawn and AgentHandle

- [ ] **C27** — SpawnAgentTool launches the child via tokio::spawn and returns immediately with agent_id and registry path.
- [ ] **C28** — SpawnAgentTool accepts an optional profile parameter and builds the child LoopContext via from_profile() when provided.
- [ ] **C29** — Child agent receives filtered tool definitions from SubAgentExecutor's allow-list, not an empty slice.
- [ ] **C30** — AgentHandle stored as ToolContext extension keyed by agent_id, providing status watch receiver and InboundSender.
- [ ] **C31** — InboundChannel created for each child agent; parent holds the sender via AgentHandle.
- [ ] **C32** — Child agent drains InboundChannel at tool boundaries using the existing Steer/FollowUp drain logic.

## Reactive Wait and Completion

- [ ] **C33** — WaitAgentTool uses tokio::sync::watch channel subscription instead of 50ms polling.
- [ ] **C34** — WaitAgentTool supports waiting on multiple agents via tokio::select! for first-to-finish.
- [ ] **C35** — Child sends completion notification to parent's mailbox with trigger_turn: true on reaching terminal status.
- [ ] **C36** — close_agent performs DFS shutdown of all descendants before transitioning the target.

## Sub-Agent Observability

- [ ] **C37** — Sub-agents can be configured with EventType::Progress schema via EventSchemaSet for structured status updates.
- [ ] **C38** — tool_use_description on sub-agent tool calls is accessible to the parent for progress visibility.
- [ ] **C39** — Sub-agent's EventStore is linked to the parent's session for audit trail purposes.
