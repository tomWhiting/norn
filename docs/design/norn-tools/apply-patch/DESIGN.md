---
type: design
cluster: norn-tools/apply-patch
title: "Apply Patch: Structural Patch Resolution"
---

# Apply Patch: Structural Patch Resolution

## Intention

Apply Patch exists so that model-generated code changes apply reliably regardless of line-number accuracy, and so that patches can serve as a coordination primitive between parent and child agents. When this is done, a model can generate a standard unified diff and the tool resolves where changes belong using structural understanding of the code, not brittle line addressing. A parent model can receive patches from forks and subagents, inspect them, dry-run them, edit them, and apply them with full control over the blast radius.

## Problem

Models generate unified diffs with wrong line numbers. They count lines from stale reads, miscalculate @@ headers, and produce context lines that have drifted since the file was last read. The current tool handles this with a three-tier recovery (header correction, context search, entity-guided placement), but the tiers are ordered wrong: the strongest signal (structural entity matching) is the last resort, while the weakest signal (line numbers) is tried first.

Beyond application accuracy, there is no way for a parent model to preview, inspect, or compose patches from child agents. Fork results arrive as opaque text. The parent cannot dry-run a child's changes, check for conflicts between two children's patches, or send a patch back for amendment. Patches are fire-and-forget.

Finally, the tool cannot create new files. When a model generates a standard unified diff with `--- /dev/null` (new file creation) alongside modifications to existing files, the entire patch is rejected. This blocks a common pattern: "add a new module and wire it into the existing code."

## Solution

### D1: Entity-first resolution order

Reverse the tier priority. For each hunk:

1. Read the @@ semantic anchor (the text after the second @@, e.g., `fn process_event`). Use libyggd's `ast::extract_entities()` to find the named entity in the file. Scope the context search to that entity's byte range.
2. If no semantic anchor or entity not found, fall back to full-file context-anchored search (exact match, then whitespace-insensitive, then trim-insensitive).
3. If context search fails, fall back to header correction + diffy application at the stated line numbers.

This inverts the current order. The strongest signal (structural identity) is tried first. Line numbers are the last resort, not the first attempt.

### D2: libyggd AST integration

Replace Norn's parallel tree-sitter integration (`tools/ast.rs`: `SupportedLanguage` enum with 5 languages, `collect_named_containers`, `find_entity_range`) with libyggd's `ast::extract_entities()`. This provides:

- 18 languages (Rust, TypeScript, Python, Go, Java, C, C++, and 11 more) instead of 5
- Qualified name resolution (methods within impl blocks, nested classes)
- EntityKind with 28 variants for precise matching
- Content and structural hashing
- Entity caching

Norn gains an optional dependency on libyggd behind a cargo feature flag (`libyggd-ast`). When enabled, entity extraction calls `libyggd::ast::extract_entities()`. When disabled, the tool falls back to context search and diffy (tiers 2-3 of the current implementation without entity guidance). The feature flag avoids pulling libyggd's full dependency tree (gitoxide, gix-merge, etc.) into norn by default. The `norn-cli` crate enables the feature; library consumers choose whether to opt in.

The integration point is a trait: `trait EntityExtractor: Send + Sync { fn extract(&self, source: &str, path: &Path) -> Option<Vec<Entity>> }`. The libyggd implementation is one concrete impl. The tool holds `Option<Arc<dyn EntityExtractor>>` — None when the feature is disabled, Some when enabled. This keeps the tool testable without libyggd.

### D3: New file creation support

When a patch block has `--- /dev/null` as its source:

- Pre-validate skips the file-exists and read-before-edit checks for that block
- Execute starts from an empty string instead of reading the file
- Parent directories are created if needed (same behavior as the Write tool)
- AST validation runs on the new content before writing
- The `/dev/null` source is detected during block parsing, not as a special case in pre-validate

File deletion (`+++ /dev/null`) is supported symmetrically: the target file is removed after validation confirms no other hunks in the same patch depend on it. Deletion specifics:

- Pre-validate checks that no later hunk in the same patch targets the file being deleted. If hunk B modifies file A and hunk C deletes file A, the patch is rejected with a clear error.
- Execute captures the file's full content as before-content before removing it.
- Undo for a deleted file recreates it from stored before-content (including restoring the file's original directory structure).
- Convention checks do not run on deleted files (there is no post-mutation content to validate).
- The deletion is recorded in the action log's mutation ledger as status "deleted" with before-content stored for undo.

### D4: Dry-run mode

A `dry_run` parameter that resolves all hunks, validates the staged content, and reports the full result without writing to disk.

Dry-run output includes:

- Per-file resolution details: which tier resolved each hunk, entity matched, line drift
- AST validation result (pass/fail with diagnostics)
- Blast radius: lines added, removed, modified per file; containing symbols
- File length impact (before/after line counts)
- Diagnostics preview: what DiagnosticsPostCheck would report
- Required follow-on edits (when LSP integration is available): new errors introduced by the staged change in dependent code

Dry-run results are stored in the action log (see action-log design) and can be applied via the follow-up tool without re-specifying the patch.

### D5: Strict mode

A `mode` parameter with three values:

- `auto` (default): entity-first resolution with full tier fallback, same behavior as today but reordered
- `strict`: only apply when context lines match exactly at the stated line position; if structural matching would have succeeded, report what it found and offer a follow-up action
- `structural`: skip diffy entirely, require entity resolution for every hunk

Strict mode is a confidence tool. The model can use it to verify its own accuracy: "did I get the line numbers right?" If strict fails but structural would have succeeded, the result includes:

```json
{
  "applied": false,
  "mode": "strict",
  "structural_alternative": {
    "would_apply": true,
    "entity": "fn process_event",
    "entity_range": [142, 155],
    "matched_at_line": 147,
    "drift": 5,
    "confidence": "high"
  },
  "follow_up_id": "ap-xxxx"
}
```

The model can then use the follow-up tool to apply with structural matching without re-generating the patch.

### D6: Patch artifacts

Patches can be assigned an artifact ID on creation. Artifact IDs allow:

- Referencing a patch without re-generating it (token savings for large patches)
- Inspecting individual files or hunks within a patch
- Editing specific hunks before applying
- Composing multiple patches (detecting conflicts between them)

Artifact lifecycle:

1. A fork or subagent returns its result as a patch artifact
2. The parent model receives the artifact ID and a summary (files, hunks, blast radius)
3. The parent can `inspect_patch(id)`, `inspect_patch(id, file: "path")`, or `inspect_patch(id, hunk: 3)`
4. The parent can `edit_patch(id, file: "path", hunk: 3, new_content: "...")` to amend before applying
5. The parent can `apply_patch(artifact_id: id, mode: "auto", dry_run: true)` to preview
6. The parent can `apply_patch(artifact_id: id)` to commit

Artifacts are stored in the session event stream. They persist across turns but not across sessions (they reference working-tree state).

### D7: Fork/subagent coordination (deferred — separate design)

Fork result packaging, multi-fork patch composition, and entity-level conflict detection are deferred to a separate design doc at `docs/design/norn-tools/fork-patches/`. That design depends on D6 (patch artifacts) but D6 does not depend on it. Artifacts work as a single-agent feature (generate patch, get ID, inspect, edit, dry-run, apply) without fork coordination.

### D8: Committed/failed transparency

When a patch is applied but post-validation (DiagnosticsPostCheck) fails, the result must include BOTH the structured output (committed: true, files_modified, diagnostics) AND the post-validation errors. The current behavior of discarding the structured output and returning only `{"error": "post-validation failed: ..."}` is a bug.

For Gate-mode tools where the file is already on disk when post-validation runs, the tool result must be honest about the committed state. The model must know whether the file was modified regardless of whether diagnostics passed. DiagnosticsPostCheck should use Report mode for post-commit checks, or the registry should include the structured output alongside the error.

## Prerequisites

**PR0: Lifecycle transparency fix.** `ToolError::PostValidationFailed` must carry an optional `committed_output: Option<serde_json::Value>` so that when post-validation fails on a committed mutation, the model sees both the error and the committed state. This is a cross-cutting fix in `infra.rs` and `tool_dispatch.rs` that all five norn-tools designs depend on. Without it, blocking convention checks and diagnostic failures hide whether the file was modified.

**PR1: PostCheckResult type.** `RuntimePostValidateCheck::check()` returns `PostCheckResult { outcome: PostValidateOutcome, advisories: Vec<Advisory> }` instead of bare `PostValidateOutcome`. Required by the conventions design for advisory handling.

## Goals

G1. A unified diff with wrong line numbers applies correctly when the @@ semantic anchor identifies the target entity.

G2. Entity resolution supports 18 languages via libyggd's AST extraction.

G3. Patches that create new files (`--- /dev/null`) apply without blocking the entire multi-file patch.

G4. Dry-run mode previews blast radius, diagnostics, and resolution details without modifying disk.

G5. Strict mode reports what structural matching would have done when exact matching fails.

G6. Patch artifacts can be assigned IDs, inspected, edited, dry-run, and applied without re-generating the patch text.

G7. The tool result always reports committed state accurately, even when post-validation fails.

## Non-Goals

NG1. Inventing a new patch format. Models generate standard unified diffs. The tool gets smarter about resolving them, it does not require models to generate a different format.

NG2. Automatic conflict resolution for multi-fork composition. Entity-level conflict detection is in scope. Automatic resolution (picking the right version) is not — that is the parent model's job.

NG3. LSP-powered follow-on edit detection in v1. Dry-run reports blast radius from static analysis (AST, diagnostics). LSP integration for "this signature change requires 3 call-site updates" is designed for but not implemented initially.

NG4. Patch persistence across sessions. Artifacts reference working-tree state and are session-scoped.

## Constraints

CO1. The patch input format remains standard unified diff. No proprietary format required from models.

CO2. Entity resolution must not add perceptible latency to simple patches. libyggd entity extraction should be lazy (only invoked when the @@ anchor contains a name).

CO3. Dry-run must produce identical resolution to actual application. The same code path runs; only the disk-write step is skipped.

CO4. Artifact storage uses the existing session event stream. No new storage backend.

CO5. File creation via `--- /dev/null` must create parent directories, same as the Write tool.

CO6. The tool must remain usable without libyggd. libyggd is an optional dependency behind a cargo feature flag (`libyggd-ast`). When disabled, entity-first resolution (D1 tier 1) is skipped and the tool starts at context-anchored search (current tier 2 behavior). The `EntityExtractor` trait allows testing with mock implementations. libyggd enhances but does not gate.
