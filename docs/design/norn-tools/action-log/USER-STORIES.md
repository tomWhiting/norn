# Norn-Tools/Action-Log — User Stories

## AI Agent — Reviewing Prior Actions

**S1.** As an AI agent, I want to list all tool calls in the session with one-line outcome summaries so that I can quickly see what I have done without re-reading the full conversation.

**S2.** As an AI agent, I want to filter the action list by tool name and outcome so that I can find specific failures or edits without scanning every entry.

**S3.** As an AI agent, I want to drill into a specific tool call by ID to see the full structured result so that I can re-examine details that context compaction removed.

**S4.** As an AI agent, I want to retrieve Level 3 full context for a tool call including before/after file content so that I can debug a mutation that produced unexpected results.

## AI Agent — Discovering Follow-Up Actions

**S5.** As an AI agent, I want to query follow-up actions from prior tool results so that I can act on deferred options even after the original result scrolled out of context.

**S6.** As an AI agent, I want expired follow-up actions filtered out automatically so that I do not attempt actions whose preconditions have changed.

**S7.** As an AI agent, I want follow-up actions to include the original tool_call_id and action name so that I can pass them directly to the follow-up tool without re-specifying arguments.

## AI Agent — Tracking File Mutations

**S8.** As an AI agent, I want to query which files I modified during this session so that I can assess blast radius without relying on git status (which shows other agents' changes too).

**S9.** As an AI agent, I want the mutation ledger to detect when a file I edited was reverted externally so that I know my change is no longer in effect.

**S10.** As an AI agent, I want mutation ledger entries to include diff stats (lines added/removed) so that I can estimate the scope of my changes at a glance.

## Human Developer — Auditing Agent Activity

**S11.** As a developer reviewing an agent's session, I want the action log to persist for the session lifetime independently of compaction so that I can audit what the agent did even if its context was heavily compacted.

**S12.** As a developer, I want the action log stored in the existing EventStore with no new file formats so that existing tooling for inspecting session events works without modification.
