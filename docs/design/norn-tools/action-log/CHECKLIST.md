# Norn-Tools/Action-Log — Checklist

## Types — Detail Levels and Outcome

- [ ] **C1** — ActionLogEntry struct holds Level 1 fields: tool_name, tool_call_id, tool_use_description, timestamp, outcome, summary_line
- [ ] **C2** — Outcome enum with Success, Error, Blocked variants
- [ ] **C3** — Level 2 detail includes full tool output JSON, input arguments, resolution details, duration, and follow-up actions
- [ ] **C4** — Level 3 full context includes raw tool output before compaction, pre-validate and post-validate outcomes, DiagnosticsPostCheck results, caught-and-recovered errors, and before/after file content for mutation tools
- [ ] **C5** — Level 1 summary computed at tool-call completion and attached to the event
- [ ] **C6** — Level 2 structured result stored as the tool output already in EventStore — no duplication
- [ ] **C7** — Level 3 before-content for mutation tools captured eagerly at register_follow_ups time (FollowUpAction carries its own before-content)
- [ ] **C8** — Level 3 for non-mutation tools populated lazily on first context query

## Query Tool — Interface and Filters

- [ ] **C9** — action_log tool registered with query parameter accepting list, detail, context, mutations, follow_ups values
- [ ] **C10** — filter object supports tool (tool name), outcome (success/error/blocked), file (affected file path), since (tool_call_id), and last (count) fields
- [ ] **C11** — list query returns Level 1 summaries matching the filter
- [ ] **C12** — detail query requires call_id parameter and returns Level 2 for that specific tool call
- [ ] **C13** — context query requires call_id parameter and returns Level 3 for that specific tool call
- [ ] **C14** — follow_ups query returns follow-up actions matching the filter with expired actions filtered out at query time
- [ ] **C15** — mutations query returns mutation ledger entries (see Mutation Ledger section)
- [ ] **C16** — Invalid query values or missing required parameters (call_id for detail/context) return structured errors

## Follow-Up Action Storage

- [ ] **C17** — Follow-up actions stored on action log entries with action_id, description, tool, args, expires, and confidence fields
- [ ] **C18** — Follow-up actions indexed by tool_call_id + action name for O(1) lookup
- [ ] **C19** — follow_ups query returns only unexpired follow-up actions — expired ones filtered out at query time
- [ ] **C20** — Expiry checking at query time compares file modification timestamp or content hash against stored value

## Mutation Ledger

- [ ] **C21** — MutationLedgerEntry struct tracks file_path, operation (created/modified/deleted), first_tool_call_id, last_tool_call_id, revert_status, and diff_stats (lines added/removed)
- [ ] **C22** — Mutation ledger is a derived view over action log entries — not a separate store
- [ ] **C23** — External revert detection: at query time, hash each entry's file and compare against post-mutation hash recorded at tool-call completion
- [ ] **C24** — If file hash differs and no subsequent tool call in the log touched that file, entry marked externally_reverted
- [ ] **C25** — No filesystem watching or polling — detection only at query time
- [ ] **C26** — Mutation ledger shows only mutations from the current session's tool calls, not pre-existing dirty files or changes from other agents

## Storage — Event Stream Integration

- [ ] **C27** — Action log data stored as events in the session's EventStore — no new storage backend
- [ ] **C28** — Structured metadata (Level 1 summary, follow-up actions) attached to existing tool-call events
- [ ] **C29** — Storage uses existing EventStore and PersistenceSink exclusively — no new database or file format

## Compaction Interaction

- [ ] **C30** — Action log retains full data regardless of context compaction state
- [ ] **C31** — Agent can drill into any prior tool call via detail or context query even if the tool result was compacted from conversation
- [ ] **C32** — action_log tool output is compact by design — Level 1 is a few tokens per entry, safe to call frequently without bloating context

## Constraints

- [ ] **C33** — Level 1 list query for 100 tool calls produces under 1000 tokens of output (CO1)
- [ ] **C34** — Follow-up action expiry checked at query time, not maintained in real-time (CO2)
- [ ] **C35** — Level 3 file snapshots for mutation tools are opt-in or lazily populated — not eagerly stored for every edit (CO3)
- [ ] **C36** — action_log tool queries logged at Level 1 only — no Level 2/3 self-referential entries that create recursive noise (CO4)
- [ ] **C37** — No new database or file format — storage via existing EventStore and PersistenceSink only (CO5)

## Prerequisites

- [ ] **C38** — ToolError::PostValidationFailed carries an optional committed_output field so that action log entries for failed-but-committed mutations include the structured output (PR0)
