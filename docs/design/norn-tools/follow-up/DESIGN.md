---
type: design
cluster: norn-tools/follow-up
title: "Follow-Up: Deferred Action Execution"
---

# Follow-Up: Deferred Action Execution

## Intention

The Follow-Up tool exists so that an agent can act on deferred options from prior tool results without re-generating the original tool call. When this is done, a strict-mode patch that failed can be retried with structural matching in a single call. A dry-run patch can be applied. A committed edit can be undone. All without the model re-specifying arguments, re-reading files, or re-generating diffs.

## Problem

Tool results frequently present the agent with follow-up options:

- "Patch failed in strict mode. Structural matching found the target at line 147 (drift: 5). Re-run with mode: structural to apply."
- "Dry-run complete. 3 files, 7 hunks. Re-run with dry_run: false to apply."
- "old_string matched at 2 locations. Specify occurrence: 1 or 2."
- "Edit committed. Undo available."

Today, the agent must re-generate the entire tool call to act on any of these. For apply_patch, that means re-emitting the full diff text (potentially hundreds of lines). For edit, it means re-specifying old_string and new_string. This wastes tokens and introduces re-generation errors (the model might subtly change the diff when re-generating it).

The follow-up tool eliminates re-generation by referencing the prior tool call's ID and selecting a named follow-up action.

## Solution

### D1: Follow-up by tool_call_id + action name

The follow-up tool takes the original tool_call_id and the name of the follow-up action to execute:

```
follow_up({
  "tool_call_id": "toolu_01ABC...",
  "action": "apply_structural"
})
```

The tool:

1. Looks up the tool_call_id in the action log
2. Finds the named action in that call's registered follow-up actions
3. Checks expiry conditions (e.g., target file not modified since the original call)
4. Retrieves the original tool call's arguments from the event store
5. Applies the follow-up's argument overrides (e.g., `mode: "structural"`)
6. Invokes the target tool with the modified arguments
7. Returns the result as if the target tool had been called directly

The tool_call_id is the existing tool use ID that every tool call already carries. No separate ID system needed. The action name is a short string declared by the tool ("apply_structural", "undo", "apply_at_occurrence_2").

### D2: Follow-up actions in tool results

When a tool completes, its result includes available follow-up actions:

```json
{
  "kind": "edit_committed",
  "committed": true,
  "path": "src/handler.rs",
  "follow_ups": [
    {
      "action": "undo",
      "description": "Revert src/handler.rs to pre-edit content",
      "expires": "file_modified:src/handler.rs"
    }
  ]
}
```

For a strict-mode patch failure:
```json
{
  "kind": "patch_strict_failed",
  "committed": false,
  "follow_ups": [
    {
      "action": "apply_structural",
      "description": "Apply using structural matching (entity: fn process_event, drift: 5 lines)",
      "expires": "file_modified:src/handler.rs"
    },
    {
      "action": "apply_auto",
      "description": "Apply using auto mode (full tier fallback)",
      "expires": "file_modified:src/handler.rs"
    }
  ]
}
```

The model sees the tool_call_id (from the tool use envelope) and the follow_ups array (in the result). To act: `follow_up({ tool_call_id: "...", action: "apply_structural" })`.

### D3: Undo as a follow-up action

Mutation tools (edit, write, apply_patch) register an "undo" follow-up action on successful commits. The undo action restores the file(s) to their pre-mutation state.

For edit and write (single file):
```json
{
  "action": "undo",
  "description": "Revert src/handler.rs to pre-edit content",
  "expires": "file_modified:src/handler.rs"
}
```

For apply_patch (multi-file), both all-or-nothing and per-file follow-ups are registered:
```json
{
  "action": "undo",
  "description": "Revert all 3 files to pre-patch content",
  "expires": "any_file_modified"
},
{
  "action": "undo_file:src/handler.rs",
  "description": "Revert only src/handler.rs to pre-patch content",
  "expires": "file_modified:src/handler.rs"
},
{
  "action": "undo_file:src/config.rs",
  "description": "Revert only src/config.rs to pre-patch content",
  "expires": "file_modified:src/config.rs"
}
```

All-or-nothing undo uses Yggdrasil's operation tracking (see D8) to atomically revert all files. Per-file undo uses stored before-content directly (not the operation revert, which is all-or-nothing). Both fall back to stored content when Yggdrasil tracking is not available.

### D4: Expiry checking

Follow-up actions can expire when preconditions change:

- `file_modified:<path>`: expires if the specified file has been modified since the original call
- `any_file_modified`: expires if any file in the patch has been modified
- `turn_scoped`: expires at the end of the current turn
- `never`: does not expire (rare — most follow-ups depend on file state)

Expiry is checked at execution time, not at storage time. This is cheap: compare the file's modification timestamp or content hash against the stored value.

When a follow-up has expired, the tool returns an error explaining why:
```json
{
  "error": "follow-up expired: src/handler.rs was modified after the original call",
  "tool_call_id": "toolu_01ABC...",
  "action": "undo",
  "suggestion": "re-read the file and re-generate the edit"
}
```

### D5: Argument override

Follow-up actions store argument overrides, not complete argument sets. The execution merges the override with the original call's arguments:

```
original args: { patch: "...", mode: "strict", dry_run: false }
follow-up override: { mode: "structural" }
merged args: { patch: "...", mode: "structural", dry_run: false }
```

Large arguments (patch text, file content, old_string) are never re-transmitted. They are read from the action log's stored copy of the original tool call.

### D6: Discovery via action log

Follow-up actions are discoverable in two ways:

1. **Immediate**: the tool result includes the follow_ups array. The model sees it right away.
2. **Later**: if context compaction removed the result, the model queries the action log: `action_log({ query: "follow_ups", filter: { tool: "apply_patch", outcome: "error" } })`. The action log returns only unexpired follow-up actions — expired ones are filtered out at query time.

The follow-up tool does not list or search actions. The action log handles discovery. The follow-up tool only executes.

### D7: Cross-tool applicability

The follow-up pattern is generic. Any tool can register follow-up actions in its result:

**apply_patch**: apply_structural, apply_auto, undo, apply_dry_run (from dry-run result)
**edit**: undo, apply_at_occurrence_N, apply_with_allow_broken_ast
**write**: undo
**search**: read_result_N, refine_query
**bash**: rerun, rerun_with_timeout

Tools register follow-up actions as a new lifecycle phase (see D9). The action log indexes them. The follow-up tool executes them.

### D8: Yggdrasil operation tracking

Mutation tools record their changes as Yggdrasil operations. Each tool call that modifies files creates an operation entry in libyggd's operation log with:

- Operation ID (maps to tool_call_id)
- Files modified with before/after content hashes
- Timestamp
- Agent identity (from AgentEventSender)

Each FollowUpAction that represents an undo carries a before-content source:

```rust
enum BeforeContentSource {
    YggdrasilOp(OperationId),
    StoredContent(HashMap<PathBuf, String>),
    Unavailable,
}
```

`YggdrasilOp` is preferred when Yggdrasil tracking is active. `StoredContent` is captured eagerly at register_follow_ups time from the original content the tool read during execute(). `Unavailable` means neither source exists — undo returns a clear error rather than failing silently.

When YggdrasilOp is available, undo uses `libyggd::ops::revert_operation(operation_id)` to atomically restore all files to their pre-operation state. This is more robust than storing raw content because:

- It handles multi-file operations atomically
- It integrates with Yggdrasil's existing operation audit trail
- It works even if the action log's Level 3 content snapshots have been compacted
- It provides a foundation for operation-level git tracking (annotated commits per operation)

When Yggdrasil tracking is not available (e.g., norn used as a library outside a Yggdrasil repo), undo falls back to writing the stored original content from the action log.

### D9: Follow-up as tool lifecycle extension

Follow-up actions are a new phase in the tool lifecycle:

Current: pre-validate → execute → post-validate → on-success
Extended: pre-validate → execute → post-validate → on-success → **register-follow-ups**

The `register_follow_ups` phase runs after on-success (or after error, for failure-recovery follow-ups). It inspects the tool output and the execution context to determine what follow-up actions to offer. It returns a `Vec<FollowUpAction>` that the registry attaches to the tool result.

This phase is optional. Tools that do not register follow-ups behave identically to today.

## Prerequisites

**PR0: Lifecycle transparency fix.** `ToolError::PostValidationFailed` must carry an optional `committed_output` so that follow-up actions on failed-but-committed mutations can reference the structured output (committed state, files modified). Without this, the follow-up tool cannot determine whether a prior tool call's mutation landed.

**PR1: PostCheckResult type.** Required by conventions for advisory handling. Follow-up actions may be registered on advisory results (e.g., "diagnostics found warnings — run clippy to review").

## Goals

G1. A follow-up action from a prior tool result can be executed by providing the tool_call_id and action name, without re-specifying the original arguments.

G2. Mutation tools offer an undo follow-up action backed by Yggdrasil operation tracking.

G3. Follow-up actions expire gracefully when preconditions change, with clear error messages.

G4. Large arguments (patch text, file content) are never re-transmitted through the model — they are read from the action log's stored copy.

G5. The follow-up mechanism is generic across all tools, not specific to apply_patch.

G6. Follow-up actions are discoverable through the action log, filtering out expired actions, regardless of context compaction state.

G7. Yggdrasil operation tracking provides atomic multi-file undo and integrates with the operation audit trail.

## Non-Goals

NG1. Automatic follow-up chains. The follow-up tool executes one action. If the result produces new follow-ups, the agent decides whether to continue. No automatic chaining.

NG2. Follow-up across sessions. Follow-up actions reference in-session state (file contents, tool call arguments). They are session-scoped.

NG3. Follow-up as a replacement for tool retries. If a tool fails transiently (network error, timeout), the retry mechanism in the runner handles it. Follow-ups are for alternative strategies, not retries of the same strategy.

## Constraints

CO1. The follow-up tool must validate that the referenced tool_call_id exists, the named action is registered, and the action has not expired before executing. Invalid or expired references produce clear errors.

CO2. Argument override merging must be deterministic. The override replaces specific fields; all other fields come from the original call. No deep-merge ambiguity.

CO3. The target tool's full lifecycle runs when invoked via follow-up. Pre-validate, execute, post-validate, on-success, register-follow-ups — all phases fire. The follow-up tool is a dispatch mechanism, not a shortcut that bypasses validation.

CO4. Follow-up execution is logged in the action log as a new entry with a reference to the original tool_call_id. The action log shows the chain: original call → follow-up.

CO5. The follow-up tool itself does not produce follow-up actions. No recursive follow-ups. The target tool's result may produce new follow-ups, but those are attributed to the target tool.

CO6. Yggdrasil operation tracking is optional. When libyggd is not available or the directory is not a Yggdrasil repo, undo falls back to stored content. The design must not require Yggdrasil for correctness.
