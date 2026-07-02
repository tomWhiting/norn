# Norn-Tools/Apply-Patch — Checklist

## Entity-First Resolution (D1)

- [ ] **C1** — Hunk resolution tries entity-guided placement first (tier 1), then context-anchored search (tier 2), then header correction + diffy (tier 3)
- [ ] **C2** — Tier 1 reads the @@ semantic anchor text (after the second @@) and uses it to locate the target entity
- [ ] **C3** — Tier 1 scopes context search to the matched entity's byte range when entity is found
- [ ] **C4** — Tier 2 full-file context-anchored search runs exact match, then whitespace-insensitive, then trim-insensitive
- [ ] **C5** — Tier 3 header correction + diffy is the last resort, not the first attempt
- [ ] **C6** — Resolution tier used is reported per-hunk in the tool result

## libyggd AST Integration (D2)

- [ ] **C7** — norn crate has an optional dependency on libyggd behind a cargo feature flag named libyggd-ast
- [ ] **C8** — EntityExtractor trait defined with fn extract(&self, source: &str, path: &Path) -> Option<Vec<Entity>>
- [ ] **C9** — ApplyPatchTool holds Option<Arc<dyn EntityExtractor>> — None when libyggd-ast feature disabled, Some when enabled
- [ ] **C10** — libyggd-ast feature provides a concrete EntityExtractor implementation that calls libyggd::ast::extract_entities()
- [ ] **C11** — norn-cli crate enables the libyggd-ast feature
- [ ] **C12** — When libyggd-ast is disabled, tier 1 is skipped and resolution starts at tier 2 (context-anchored search)
- [ ] **C13** — Existing SupportedLanguage enum and parallel tree-sitter code in ast.rs replaced by EntityExtractor trait
- [ ] **C14** — Entity extraction supports 18+ languages via libyggd (Rust, TypeScript, Python, Go, Java, C, C++, and more)

## New File Creation and Deletion (D3)

- [ ] **C15** — Patches with --- /dev/null as source create new files
- [ ] **C16** — Pre-validate skips file-exists and read-before-edit checks for /dev/null source blocks
- [ ] **C17** — Execute starts from an empty string for /dev/null source blocks instead of reading the file
- [ ] **C18** — Parent directories are created if needed for new files (same behavior as Write tool)
- [ ] **C19** — AST validation runs on new file content before writing
- [ ] **C20** — /dev/null source detected during block parsing, not as a special case in pre-validate
- [ ] **C21** — Patches with +++ /dev/null as target delete the target file
- [ ] **C22** — Pre-validate rejects patches where a later hunk modifies a file that an earlier hunk deletes
- [ ] **C23** — Execute captures full file content as before-content before deleting
- [ ] **C24** — Convention checks do not run on deleted files
- [ ] **C25** — Deletion recorded in action log mutation ledger as status deleted with before-content

## Dry-Run Mode (D4)

- [ ] **C26** — dry_run boolean parameter added to apply_patch input schema
- [ ] **C27** — Dry-run resolves all hunks and validates staged content without writing to disk
- [ ] **C28** — Dry-run output includes per-file resolution details: tier used, entity matched, line drift
- [ ] **C29** — Dry-run output includes AST validation result (pass/fail with diagnostics)
- [ ] **C30** — Dry-run output includes blast radius: lines added, removed, modified per file and containing symbols
- [ ] **C31** — Dry-run output includes file length impact (before/after line counts)
- [ ] **C32** — Dry-run output includes diagnostics preview matching what DiagnosticsPostCheck would report
- [ ] **C33** — Dry-run produces identical resolution to actual application — same code path, only disk-write skipped
- [ ] **C34** — Dry-run results stored in the action log

## Strict Mode (D5)

- [ ] **C35** — mode parameter added to apply_patch input schema with values auto, strict, structural
- [ ] **C36** — auto mode (default) uses entity-first resolution with full tier fallback
- [ ] **C37** — strict mode only applies when context lines match exactly at the stated line position
- [ ] **C38** — strict mode reports what structural matching would have found when exact matching fails
- [ ] **C39** — strict mode structural_alternative includes would_apply, entity, entity_range, matched_at_line, drift, confidence
- [ ] **C40** — structural mode skips diffy entirely and requires entity resolution for every hunk
- [ ] **C41** — Failed strict-mode result includes a follow-up action ID for applying with structural matching

## Patch Artifacts (D6)

- [ ] **C42** — Patches can be assigned an artifact ID via an artifact_id parameter
- [ ] **C43** — Artifact storage uses the existing session event stream
- [ ] **C44** — Artifacts persist across turns but not across sessions
- [ ] **C45** — inspect_patch follow-up allows viewing a stored artifact by ID, optionally filtered by file or hunk index
- [ ] **C46** — edit_patch follow-up allows amending a specific hunk in a stored artifact before applying
- [ ] **C47** — apply_patch accepts artifact_id to apply a previously stored artifact
- [ ] **C48** — Artifact summary includes file count, hunk count, and blast radius estimate

## Committed/Failed Transparency (D8)

- [ ] **C49** — Tool result always reports committed state accurately even when post-validation fails (depends NTI-001)
- [ ] **C50** — Model receives both the error reason and the committed output (files_modified, diagnostics, committed flag) when Gate-mode post-validation fails

## Constraints

- [ ] **C51** — Patch input format remains standard unified diff — no proprietary format required from models (CO1)
- [ ] **C52** — Entity resolution does not add perceptible latency to simple patches — libyggd entity extraction is lazy, invoked only when @@ anchor contains a name (CO2)
- [ ] **C53** — Dry-run produces identical resolution to actual application — same code path, disk-write step skipped (CO3)
- [ ] **C54** — File creation via /dev/null creates parent directories same as Write tool (CO5)
- [ ] **C55** — Tool remains usable without libyggd — libyggd-ast feature flag is optional, entity-first tier skipped when disabled (CO6)
