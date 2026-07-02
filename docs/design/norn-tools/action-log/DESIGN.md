---
type: design
cluster: norn-tools/action-log
title: "Action Log: Tool Call History with Drill-Down"
---

# Action Log: Tool Call History with Drill-Down

## Intention

The Action Log exists so that an agent can look back at what it has done, drill into the details of any prior tool call, and discover follow-up actions that were available but not taken. When this is done, the agent has a queryable memory of its own actions that is cheaper than re-reading the full conversation history and richer than what compacted context retains.

## Problem

Agents lose track of their own actions as conversations grow. Context compaction summarizes or removes tool results to save tokens, but the agent may need to:

- Check whether an edit was actually committed or rolled back
- Review the blast radius of a change made 20 turns ago
- Find out which files were modified during the session
- Discover that a strict-mode patch failed but structural matching would have worked
- Re-inspect a fork's patch artifact before deciding to apply it

Today, the agent either re-reads the full conversation (expensive), trusts compacted summaries (lossy), or re-runs the tool (wasteful). There is no lightweight way to query "what did I do and what happened?"

Additionally, some tool results offer follow-up options (e.g., "strict match failed, structural would work — want to apply?") but these options are lost when the tool result scrolls out of context. The agent has to re-generate the full tool call to get back to the same decision point.

## Solution

### D1: Three-level detail hierarchy

The action log stores every tool call at three levels of detail. The agent queries the level it needs.

**Level 1: Summary line**
- Tool name
- Tool call ID
- Tool use description (the model-provided description)
- Timestamp
- Outcome: success / error / blocked
- One-line result summary (e.g., "edit committed: src/handler.rs +5/-3" or "patch blocked: AST validation failed")

Level 1 is cheap to scan. An agent can review 50 tool calls in a few hundred tokens.

**Level 2: Structured result**
- Full tool output JSON (committed, diagnostics, blast_radius, check_overrides, etc.)
- Input arguments (what the model sent)
- Resolution details (which tier applied, entity matched, drift amount)
- Duration
- Follow-up actions available (see D3)

Level 2 is a single tool call's worth of detail. The agent drills into a specific call by ID.

**Level 3: Full context**
- Raw tool output before any compaction
- Pre-validate and post-validate outcomes
- DiagnosticsPostCheck results
- Any errors that were caught and recovered from
- The file content before and after (for mutation tools)

Level 3 is expensive and rarely needed. It exists for debugging and audit, not routine use.

### D2: Query interface

The action log is accessed via a tool:

```
action_log({
  "query": "list",           // list | detail | context
  "filter": {
    "tool": "edit",           // optional: filter by tool name
    "outcome": "error",       // optional: success | error | blocked
    "file": "src/handler.rs", // optional: filter by affected file
    "since": "tool-call-42",  // optional: everything after this call
    "last": 10                // optional: last N calls
  },
  "call_id": "tool-call-42"  // required for detail/context queries
})
```

`list` returns Level 1 summaries. `detail` returns Level 2 for a specific call. `context` returns Level 3 for a specific call.

### D3: Follow-up actions

When a tool result includes alternative outcomes or deferred options, these are stored as follow-up actions in the action log entry. Each follow-up action has:

- `action_id`: unique identifier
- `description`: human-readable description of what the action does
- `tool`: which tool would be invoked
- `args`: pre-populated arguments (the agent does not need to re-specify them)
- `expires`: whether the action is still valid (e.g., file has not been modified since)
- `confidence`: how likely the action is to succeed

Examples of follow-up actions:

**apply_patch strict failure:**
```json
{
  "action_id": "ap-strict-to-structural-xxxx",
  "description": "Apply using structural matching (entity: fn process_event, drift: 5 lines)",
  "tool": "apply_patch",
  "args": { "patch_ref": "tool-call-42", "mode": "structural" },
  "expires": "file_modified:src/handler.rs",
  "confidence": "high"
}
```

**edit ambiguous match:**
```json
{
  "action_id": "edit-location-2-xxxx",
  "description": "Apply edit at second occurrence (line 89, in fn validate_input)",
  "tool": "edit",
  "args": { "edit_ref": "tool-call-43", "occurrence": 2 },
  "expires": "file_modified:src/validator.rs",
  "confidence": "high"
}
```

**dry-run ready to apply:**
```json
{
  "action_id": "apply-dry-run-xxxx",
  "description": "Apply the dry-run patch (3 files, 7 hunks, blast radius: 42 lines)",
  "tool": "apply_patch",
  "args": { "patch_ref": "tool-call-44", "dry_run": false },
  "expires": "any_file_modified",
  "confidence": "high"
}
```

Follow-up actions are consumed by the follow-up tool (see follow-up design).

### D4: Mutation ledger

The action log includes a derived view: the mutation ledger. This tracks every file modification during the session:

- File path
- Created / modified / deleted
- First tool call that touched it
- Last tool call that touched it
- Whether it was subsequently reverted (by another tool call or externally)
- Rough diff stats (lines added/removed)

The mutation ledger is queryable via the action_log tool with `"query": "mutations"`. This answers "what files did I change?" without `git status` (which also shows pre-existing dirty files and changes from other agents in the shared working tree).

External revert detection is lazy, evaluated at query time. When the ledger is queried, each entry's file is hashed and compared against the post-mutation hash recorded when the tool call completed. If the hash differs and no subsequent tool call in the log touched that file, the entry is marked `externally_reverted`. No filesystem watching or polling — detection happens only when the agent asks.

### D5: Storage in the event stream

Action log entries are stored as events in the session's EventStore. Each tool call already produces events; the action log adds structured metadata to these events:

- Level 1 summary is computed at tool-call completion and attached to the event
- Level 2 structured result is the tool output itself (already stored)
- Level 3 full context includes pre/post file snapshots. For mutation tools that register undo follow-ups, before-content is captured eagerly at register_follow_ups time (the follow-up action carries its own before-content). For non-mutation tools (reads, searches), Level 3 is populated lazily on first query.
- Follow-up actions are attached to the event and indexed by tool_call_id + action name

No new storage backend. The action log is a query layer over existing event data with additional metadata.

### D6: Compaction interaction

When context compaction removes or summarizes tool results from the conversation, the action log retains the full data. The agent can always drill back into any tool call regardless of compaction state. This is the key value: compaction saves tokens in the conversation, but the action log preserves the detail.

The action log tool itself produces compact output (Level 1 is a few tokens per entry). It is designed to be called frequently without bloating context.

## Prerequisites

**PR0: Lifecycle transparency fix.** `ToolError::PostValidationFailed` must carry an optional `committed_output` so that action log entries for failed-but-committed mutations include the structured output. Without this, Level 2 detail for Gate-mode post-validation failures would contain only the error string, not the committed state.

## Goals

G1. An agent can list all tool calls in the session with outcome summaries in under 500 tokens.

G2. An agent can drill into any prior tool call to see the full structured result.

G3. Follow-up actions from prior tool results are discoverable and actionable regardless of compaction state.

G4. The mutation ledger answers "what files did I change?" without relying on git status.

G5. Action log data persists for the session lifetime, independent of context compaction.

## Non-Goals

NG1. Cross-session action log. The log is session-scoped. Historical analysis across sessions is a different feature.

NG2. Automatic follow-up execution. The action log stores follow-up options. The follow-up tool executes them. The agent makes the decision.

NG3. Real-time streaming of action log updates. The log is queried on demand, not pushed.

NG4. Replacing the event stream. The action log is a query layer over existing events, not a replacement.

## Constraints

CO1. Level 1 queries must be fast and token-cheap. Scanning 100 tool calls should produce under 1000 tokens of output.

CO2. Follow-up action expiry must be checked at query time, not maintained in real-time. When the agent queries a follow-up action, the log checks whether the preconditions still hold (e.g., file not modified since).

CO3. Level 3 (full context) snapshots for mutation tools must be opt-in or lazily populated. Storing before/after file content for every edit would be expensive for large files.

CO4. The action log tool must not itself appear in the action log in a way that creates recursive noise. Log queries are logged at Level 1 only.

CO5. Storage uses the existing EventStore and PersistenceSink. No new database or file format.
