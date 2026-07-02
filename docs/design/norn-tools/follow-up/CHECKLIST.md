# Norn-Tools/Follow-Up — Checklist

## Types — FollowUpAction, ExpiryCondition, BeforeContentSource

- [ ] **C1** — FollowUpAction struct in norn::tool::follow_up with action (name), description, tool (target tool name), args (pre-populated argument overrides), expires (ExpiryCondition), confidence, and before_content (BeforeContentSource) fields
- [ ] **C2** — ExpiryCondition enum in norn::tool::follow_up with FileModified(PathBuf), AnyFileModified(Vec<PathBuf>), TurnScoped, and Never variants
- [ ] **C3** — BeforeContentSource enum in norn::tool::follow_up with YggdrasilOp(OperationId), StoredContent(HashMap<PathBuf, String>), and Unavailable variants
- [ ] **C4** — FollowUpAction serializes to JSON matching the follow_ups array format in tool results (action, description, expires fields visible to model)

## Follow-Up Tool — Lookup and Dispatch

- [ ] **C5** — follow_up tool registered with tool_call_id (string) and action (string) parameters
- [ ] **C6** — Tool looks up tool_call_id in the action log and returns a structured error if not found
- [ ] **C7** — Tool finds the named action in the call's registered follow-up actions and returns a structured error if no such action exists
- [ ] **C8** — Tool checks expiry conditions before executing and returns error with reason and suggestion if expired
- [ ] **C9** — Tool retrieves original tool call's arguments from the event store
- [ ] **C10** — Tool applies the follow-up's argument overrides to the original arguments (override replaces specific fields, all others from original)
- [ ] **C11** — Tool invokes the target tool with the merged arguments
- [ ] **C12** — Result returned as if the target tool had been called directly — same output format, same structured fields

## Undo Registration on Mutation Tools

- [ ] **C13** — edit tool registers an undo follow-up action on successful commit that restores the file to pre-edit content
- [ ] **C14** — write tool registers an undo follow-up action on successful commit that restores the file to pre-write content
- [ ] **C15** — apply_patch registers an all-or-nothing undo follow-up that reverts all patched files
- [ ] **C16** — apply_patch registers per-file undo follow-ups (undo_file:<path>) for each file in the patch
- [ ] **C17** — All-or-nothing undo uses Yggdrasil operation tracking (revert_operation) when available, falls back to stored content otherwise
- [ ] **C18** — Per-file undo uses stored before-content directly (not operation revert, which is all-or-nothing)
- [ ] **C19** — Undo follow-up expires with file_modified:<path> (single file) or any_file_modified (multi-file all-or-nothing)

## Expiry Checking

- [ ] **C20** — FileModified expiry checks whether the specified file has been modified since the original call via content hash comparison
- [ ] **C21** — AnyFileModified expiry checks whether any file in the set has been modified since the original call
- [ ] **C22** — TurnScoped expiry marks the action invalid at end of the current turn
- [ ] **C23** — Never expiry means the action does not expire
- [ ] **C24** — Expiry checked at execution time, not storage time — cheap hash or timestamp comparison
- [ ] **C25** — Expired follow-up returns structured error with the reason (which file changed), the tool_call_id, the action name, and a suggestion to re-read and re-generate

## Argument Override Merging

- [ ] **C26** — Follow-up actions store argument overrides, not complete argument sets
- [ ] **C27** — Merging is deterministic: override replaces named fields, all other fields come from the original call
- [ ] **C28** — Large arguments (patch text, file content, old_string) read from the action log's stored copy of the original call — never re-transmitted through the model

## Cross-Tool Follow-Up Registration

- [ ] **C29** — apply_patch registers apply_structural and apply_auto follow-ups on strict-mode failure
- [ ] **C30** — apply_patch registers apply_dry_run follow-up when a dry-run completes successfully
- [ ] **C31** — edit registers apply_at_occurrence_N follow-ups on ambiguous multi-match
- [ ] **C32** — write registers undo follow-up on successful commit
- [ ] **C33** — bash registers rerun and rerun_with_timeout follow-ups
- [ ] **C34** — Follow-up actions in tool results include the follow_ups array visible to the model with action, description, and expires fields

## Lifecycle Extension — register_follow_ups Phase

- [ ] **C35** — Tool trait gains a register_follow_ups method as a new lifecycle phase after on-success
- [ ] **C36** — register_follow_ups also runs after error for failure-recovery follow-ups (e.g., apply_structural on strict failure)
- [ ] **C37** — register_follow_ups returns Vec<FollowUpAction>
- [ ] **C38** — Registry attaches returned follow-ups to the tool result under a follow_ups key
- [ ] **C39** — Phase is optional — default implementation returns empty Vec, tools without follow-ups are unchanged

## Yggdrasil Operation Tracking Integration

- [ ] **C40** — Mutation tools record changes as Yggdrasil operations with operation_id mapping to tool_call_id, files modified with before/after content hashes, timestamp, and agent identity
- [ ] **C41** — YggdrasilOp variant on BeforeContentSource is preferred when Yggdrasil tracking is active
- [ ] **C42** — StoredContent variant captured eagerly at register_follow_ups time from content the tool read during execute()
- [ ] **C43** — Unavailable variant returns a clear error on undo attempt — no silent failure
- [ ] **C44** — libyggd::ops::revert_operation(operation_id) atomically restores all files to pre-operation state
- [ ] **C45** — When Yggdrasil tracking is not available (norn used outside a Yggdrasil repo), undo falls back to writing stored content from the action log

## Constraints

- [ ] **C46** — follow_up tool validates tool_call_id exists, action is registered, and action has not expired before executing — invalid or expired references produce clear errors (CO1)
- [ ] **C47** — Argument override merging is deterministic — override replaces specific fields, all other fields from original call, no deep-merge ambiguity (CO2)
- [ ] **C48** — Target tool's full lifecycle runs when invoked via follow-up: pre-validate, execute, post-validate, on-success, register-follow-ups — no shortcut that bypasses validation (CO3)
- [ ] **C49** — Follow-up execution logged in action log as a new entry with a reference to the original tool_call_id — action log shows the chain (CO4)
- [ ] **C50** — follow_up tool itself does not produce follow-up actions — no recursive follow-ups; target tool's result may produce new ones attributed to the target (CO5)
- [ ] **C51** — Yggdrasil operation tracking is optional — undo falls back to stored content when libyggd unavailable or directory is not a Yggdrasil repo (CO6)

## Prerequisites

- [ ] **C52** — ToolError::PostValidationFailed carries optional committed_output so follow-up actions on failed-but-committed mutations can reference the structured output (PR0)
- [ ] **C53** — PostCheckResult type returned from RuntimePostValidateCheck::check() with outcome and advisories fields, enabling advisory follow-up registration (PR1)
