# Norn-Tools/Follow-Up — User Stories

## AI Agent — Executing Follow-Up Actions

**S1.** As an AI agent, I want to execute a follow-up action by providing the tool_call_id and action name so that I can act on a deferred option without re-generating the original tool call's arguments.

**S2.** As an AI agent, I want a clear error when a follow-up action has expired so that I know why it failed and what to do instead (re-read the file and re-generate the edit).

**S3.** As an AI agent, I want the follow-up tool to return results in the same format as the target tool so that I can process the outcome identically to a direct tool call.

**S4.** As an AI agent, I want to undo a committed edit by calling follow_up with the tool_call_id and action 'undo' so that I can revert a mistake without manually re-reading the file and writing back the original content.

## AI Agent — Recovering from Patch Failures

**S5.** As an AI agent, I want to retry a strict-mode patch with structural matching by calling a follow-up action so that I do not re-emit hundreds of lines of diff text.

**S6.** As an AI agent, I want to apply a dry-run patch result by calling a follow-up action so that I do not re-transmit the full patch content.

**S7.** As an AI agent, I want to apply an edit at a specific occurrence when the original matched multiple locations so that I can resolve ambiguity without re-specifying old_string and new_string.

## AI Agent — Discovering Available Follow-Ups

**S8.** As an AI agent, I want to see available follow-up actions in the tool result immediately after a tool call so that I can decide whether to act on them in the same turn.

**S9.** As an AI agent, I want to discover follow-up actions via the action log after context compaction removed the original result so that deferred options remain accessible regardless of conversation length.

## Tool Author — Registering Follow-Up Actions

**S10.** As a tool author, I want to register follow-up actions in a register_follow_ups lifecycle phase so that my tool can offer deferred options without mixing follow-up logic into execute().

**S11.** As a tool author, I want the register_follow_ups phase to be optional with a default empty implementation so that existing tools require no changes.

**S12.** As a tool author, I want to register follow-ups on both success and error outcomes so that failure-recovery actions (like switching from strict to structural mode) are available.

## Human Developer — Auditing Undo and Revert Operations

**S13.** As a developer reviewing an agent's session, I want follow-up executions logged in the action log with a reference to the original tool_call_id so that I can trace the chain from original call to follow-up.

**S14.** As a developer, I want undo operations backed by Yggdrasil operation tracking so that multi-file reverts are atomic and appear in the operation audit trail.

**S15.** As a developer working outside a Yggdrasil repo, I want undo to fall back to stored content so that the follow-up tool works without requiring Yggdrasil infrastructure.
