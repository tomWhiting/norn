//! `ApplyPatch` tool — applies unified-diff patches with read-before-edit
//! gating and tree-sitter AST validation.
//!
//! Supports two patch formats:
//!
//! * **Unified diff** — standard `---`/`+++` headers with `@@ -a,b +c,d @@`
//!   hunks, applied via `diffy`.
//! * **Claude Code format** — `*** Begin Patch` / `*** Update File:` /
//!   `*** End Patch` delimiters with context-based `@@` hunks. Applied via
//!   string-matching (same strategy as the Edit tool).
//!
//! Format detection is automatic: if the trimmed input starts with
//! `*** Begin Patch`, the Claude Code parser runs; otherwise the unified
//! diff parser runs.
//!
//! Both formats flow through the same lifecycle:
//!
//! 1. `pre_validate` parses the patch, extracts every target path, checks
//!    each file exists on disk, and rejects the patch if any target has
//!    not been read in this session (read-before-edit).
//! 2. `execute` applies each file's hunks in memory, runs tree-sitter AST
//!    validation on the staged contents, and either commits all files or
//!    rolls back (Gate semantics). [`ToolFlag::AllowBrokenAst`] downgrades
//!    Gate to Report.
//! 3. `on_success` registers each modified path so subsequent Edit/Write
//!    calls see them as read.
//!
//! diffy 0.5 only parses single-file patches via `Patch::from_str`. The
//! unified diff path splits the input on `^--- ` boundaries so multi-file
//! patches (each with its own `---`/`+++` header pair) work transparently.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::ast::{AstCheck, check_syntax};
use super::patch_entity::EntityExtractor;
use super::patch_parse::{
    PatchBlockKind, PatchFormat, collect_unified_additions, parse_blocks, resolve_path,
};
use super::write::syntax_error_to_diagnostic;
use crate::error::ToolError;
use crate::session::action_log::hash_content;
use crate::tool::ToolArgs;
use crate::tool::context::{ToolContext, ToolFlag};
use crate::tool::envelope::ToolEnvelope;
use crate::tool::follow_up::{BeforeContentSource, Confidence, ExpiryCondition, FollowUpAction};
use crate::tool::lifecycle::{
    CheckOverride, PostValidateMode, PostValidateOutcome, PreValidateOutcome,
};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Applies unified diff patches.
pub struct ApplyPatchTool {
    /// Optional entity extractor for tier-1 anchor resolution. `None` skips
    /// tier 1 entirely, so unified-diff hunks resolve via context search
    /// (tier 2) and header-corrected diffy (tier 3).
    extractor: Option<Arc<dyn EntityExtractor>>,
}

impl ApplyPatchTool {
    /// Constructs the tool without an entity extractor: tier-1 entity-guided
    /// resolution is skipped and unified-diff hunks resolve via context
    /// search and diffy.
    #[must_use]
    pub fn new() -> Self {
        Self { extractor: None }
    }

    /// Constructs the tool with an [`EntityExtractor`], enabling tier-1
    /// entity-guided resolution: the hunk's `@@` semantic anchor names the
    /// target entity, whose line range scopes the context search.
    #[must_use]
    pub fn with_extractor(extractor: Arc<dyn EntityExtractor>) -> Self {
        Self {
            extractor: Some(extractor),
        }
    }
}

impl Default for ApplyPatchTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Hunk-resolution mode selected by the model on the `apply_patch` call.
///
/// * [`PatchMode::Auto`] — the default entity-first tier order (entity-guided
///   → context search → diffy). Backward compatible: omitting `mode` resolves
///   here.
/// * [`PatchMode::Strict`] — apply a hunk only when its context matches
///   byte-for-byte at the exact stated `@@` line. A confidence tool: failures
///   are non-fatal and report what structural matching would have found.
/// * [`PatchMode::Structural`] — require a semantic anchor and successful
///   entity resolution for every hunk; context search is scoped to the
///   entity's range and there is no diffy fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PatchMode {
    /// Entity-first tier order, the backward-compatible default.
    #[default]
    Auto,
    /// Exact stated-line matching only; no tier fallback.
    Strict,
    /// Entity resolution required for every hunk; no diffy fallback.
    Structural,
}

#[derive(Debug, Deserialize, ToolArgs)]
struct PatchArgs {
    /// Unified-diff patch text. Supports single-file and multi-file patches.
    patch: String,
    /// Directory to resolve relative paths in the patch against.
    #[serde(default)]
    working_dir: Option<String>,
    /// Hunk-resolution mode. Defaults to [`PatchMode::Auto`] when omitted.
    #[serde(default)]
    #[tool_args(schema = {
        "type": "string",
        "enum": ["auto", "strict", "structural"],
        "default": "auto",
        "description": "Hunk-resolution mode. 'auto' (default) resolves entity-first: entity-guided placement, then context search, then header-corrected diffy. 'strict' applies a hunk only when its context matches exactly at the stated @@ line; failures are non-fatal and report whether structural matching would have succeeded — use it to verify your line numbers. 'structural' requires a semantic anchor and entity resolution for every hunk, searches context only within the entity's range, and never falls back to diffy."
    })]
    mode: PatchMode,
}

struct StagedFile {
    path: PathBuf,
    original: String,
    staged: String,
    hunks: usize,
    added: usize,
    removed: usize,
    kind: PatchBlockKind,
}

/// Reverses any disk mutations recorded in `applied` (indices into
/// `staged`). Modify → write original back; Create → unlink the file
/// we just created; Delete → recreate the file from captured before-
/// content. Rollback errors are intentionally swallowed: the function
/// is best-effort cleanup invoked from the error path of `execute`.
async fn rollback_applied(staged: &[StagedFile], applied: &[usize]) {
    for &idx in applied.iter().rev() {
        let Some(s) = staged.get(idx) else { continue };
        match s.kind {
            PatchBlockKind::Modify | PatchBlockKind::Delete => {
                let _ = tokio::fs::write(&s.path, s.original.as_bytes()).await;
            }
            PatchBlockKind::Create => {
                let _ = tokio::fs::remove_file(&s.path).await;
            }
        }
    }
}

/// Whether any `resolution_details` entry is an unapplied hunk that carries a
/// non-null `structural_alternative` — i.e. a strict failure that structural
/// matching would have resolved.
fn has_viable_strict_alternative(resolution_details: &[serde_json::Value]) -> bool {
    resolution_details.iter().any(|d| {
        d.get("applied").and_then(serde_json::Value::as_bool) == Some(false)
            && d.get("structural_alternative")
                .is_some_and(|v| !v.is_null())
    })
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn description(&self) -> &'static str {
        include_str!("guidance/apply_patch.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::FileSystem
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("guidance/apply_patch.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        PatchArgs::json_schema()
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Write
    }

    fn post_validate_mode(&self) -> PostValidateMode {
        PostValidateMode::Gate
    }

    async fn pre_validate(&self, envelope: &ToolEnvelope, ctx: &ToolContext) -> PreValidateOutcome {
        let args: PatchArgs = match serde_json::from_value(envelope.model_args.clone()) {
            Ok(a) => a,
            Err(e) => {
                return PreValidateOutcome::Block {
                    reason: format!("invalid arguments: {e}"),
                };
            }
        };
        let blocks = match parse_blocks(&args.patch) {
            Ok(b) => b,
            Err(e) => {
                return PreValidateOutcome::Block {
                    reason: format!("patch parse failed: {e}"),
                };
            }
        };

        // First pass: reject patches where a later block modifies or
        // creates a file that an earlier block deletes. Modify→Delete
        // (delete a file after editing it within the same patch) is
        // allowed; Delete→{Modify,Create} on the same path would write
        // to a path the patch just removed.
        let mut first_delete: HashMap<&str, usize> = HashMap::new();
        for (i, block) in blocks.iter().enumerate() {
            if matches!(block.kind, PatchBlockKind::Delete) {
                first_delete.entry(block.target.as_str()).or_insert(i);
                continue;
            }
            if let Some(&del_idx) = first_delete.get(block.target.as_str()) {
                let action = match block.kind {
                    PatchBlockKind::Modify => "modified",
                    PatchBlockKind::Create => "created",
                    PatchBlockKind::Delete => continue,
                };
                return PreValidateOutcome::Block {
                    reason: format!(
                        "apply_patch: file '{}' is deleted by block {del_idx} but {action} by block {i}",
                        block.target,
                    ),
                };
            }
        }

        // Second pass: per-block existence and read-before-edit gates.
        // Create blocks skip both gates (the file does not exist yet by
        // definition) but are rejected if the target already exists on
        // disk — that would silently turn a creation into a Modify.
        let effective_wd: std::path::PathBuf = match args.working_dir.as_deref() {
            Some(s) => std::path::PathBuf::from(s),
            None => ctx.working_dir(),
        };
        for block in &blocks {
            let path = resolve_path(&effective_wd, &block.target);
            match block.kind {
                PatchBlockKind::Create => {
                    if path.exists() {
                        return PreValidateOutcome::Block {
                            reason: format!(
                                "apply_patch: Create block target already exists: {}",
                                path.display()
                            ),
                        };
                    }
                }
                PatchBlockKind::Modify | PatchBlockKind::Delete => {
                    if !path.exists() {
                        return PreValidateOutcome::Block {
                            reason: format!(
                                "apply_patch: target file does not exist: {}",
                                path.display()
                            ),
                        };
                    }
                    if !ctx.has_read_file(&path) {
                        return PreValidateOutcome::Block {
                            reason: format!(
                                "apply_patch: target file not read this session: {}",
                                path.display()
                            ),
                        };
                    }
                }
            }
        }
        PreValidateOutcome::Proceed
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let started = Instant::now();
        let args: PatchArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("invalid arguments: {e}"),
            }
        })?;

        let blocks = parse_blocks(&args.patch).map_err(|e| ToolError::ExecutionFailed {
            reason: format!("patch parse failed: {e}"),
        })?;

        let mut staged_files: Vec<StagedFile> = Vec::with_capacity(blocks.len());
        let mut total_hunks = 0usize;
        let mut total_added = 0usize;
        let mut total_removed = 0usize;
        let mut all_diagnostics: Vec<serde_json::Value> = Vec::new();
        let mut resolution_details: Vec<serde_json::Value> = Vec::new();

        let effective_wd: std::path::PathBuf = match args.working_dir.as_deref() {
            Some(s) => std::path::PathBuf::from(s),
            None => ctx.working_dir(),
        };

        for block in blocks {
            let path = resolve_path(&effective_wd, &block.target);

            let (original, new_content, hunks, added, removed) = match block.kind {
                PatchBlockKind::Modify => {
                    let original = tokio::fs::read_to_string(&path).await.map_err(|e| {
                        ToolError::ExecutionFailed {
                            reason: format!("failed to read {} for patch: {e}", path.display()),
                        }
                    })?;
                    let (applied, h, a, r, resolutions) = match &block.format {
                        PatchFormat::UnifiedDiff { raw } => {
                            super::patch_apply::apply_unified_tiered(
                                raw,
                                &original,
                                &path,
                                self.extractor.as_deref(),
                                args.mode,
                            )?
                        }
                        PatchFormat::ClaudeCode { hunks: cc_hunks } => {
                            let applied = super::patch_cc::apply_cc_hunks(&original, cc_hunks)
                                .map_err(|e| ToolError::ExecutionFailed {
                                    reason: format!(
                                        "failed to apply patch to {}: {e}",
                                        path.display()
                                    ),
                                })?;
                            let h = cc_hunks.len();
                            let a: usize = cc_hunks.iter().map(|hk| hk.additions.len()).sum();
                            let r: usize = cc_hunks.iter().map(|hk| hk.removals.len()).sum();
                            (applied, h, a, r, Vec::new())
                        }
                    };
                    // Record per-hunk resolution metadata for unified-diff
                    // hunks (R5). Create and Delete blocks place no hunks
                    // against existing content, and the Claude Code format
                    // resolves with its own strategy, so neither emits
                    // resolution_details entries. Hunk index is 1-based to
                    // match the user-facing numbering in error messages.
                    for (hunk_index, res) in resolutions.iter().enumerate() {
                        resolution_details.push(serde_json::json!({
                            "file": path.to_string_lossy(),
                            "hunk_index": hunk_index + 1,
                            "tier_used": res.tier_used,
                            "entity_matched": res.entity_matched,
                            "matched_at_line": res.matched_at_line,
                            "stated_line": res.stated_line,
                            "drift": res.drift,
                            "confidence": res.confidence,
                            "applied": res.applied,
                            "failure": res.failure,
                            "structural_alternative": res.structural_alternative,
                        }));
                    }
                    (original, applied, h, a, r)
                }
                PatchBlockKind::Create => {
                    // CC format is never tagged Create at parse time
                    // (parse_blocks tags every CC block as Modify), but
                    // guard the invariant rather than panic.
                    let raw = match &block.format {
                        PatchFormat::UnifiedDiff { raw } => *raw,
                        PatchFormat::ClaudeCode { .. } => {
                            return Err(ToolError::ExecutionFailed {
                                reason: format!(
                                    "apply_patch: Claude Code format does not support file creation: {}",
                                    path.display()
                                ),
                            });
                        }
                    };
                    let (new_content, h, a) = collect_unified_additions(raw);
                    (String::new(), new_content, h, a, 0usize)
                }
                PatchBlockKind::Delete => {
                    let original = tokio::fs::read_to_string(&path).await.map_err(|e| {
                        ToolError::ExecutionFailed {
                            reason: format!(
                                "apply_patch: file expected for deletion does not exist: {}: {e}",
                                path.display()
                            ),
                        }
                    })?;
                    let removed = original.lines().count();
                    (original, String::new(), 0usize, 0usize, removed)
                }
            };

            total_hunks += hunks;
            total_added += added;
            total_removed += removed;

            // Skip AST validation on deletions — there is no
            // post-mutation content to validate.
            if !matches!(block.kind, PatchBlockKind::Delete) {
                let ast = check_syntax(&path, &new_content);
                if let AstCheck::Fail { errors } = &ast {
                    for e in errors {
                        let mut diag = syntax_error_to_diagnostic(e);
                        if let Some(map) = diag.as_object_mut() {
                            map.insert(
                                "file".to_string(),
                                serde_json::Value::String(path.to_string_lossy().into_owned()),
                            );
                        }
                        all_diagnostics.push(diag);
                    }
                }
            }

            staged_files.push(StagedFile {
                path,
                original,
                staged: new_content,
                hunks,
                added,
                removed,
                kind: block.kind,
            });
        }

        let allow_broken = ctx.has_flag(&ToolFlag::AllowBrokenAst);
        let mut check_overrides: Vec<CheckOverride> = Vec::new();

        let has_error_diagnostic = all_diagnostics
            .iter()
            .any(|d| d.get("severity").and_then(serde_json::Value::as_str) == Some("error"));

        let mode_str = match args.mode {
            PatchMode::Auto => "auto",
            PatchMode::Strict => "strict",
            PatchMode::Structural => "structural",
        };
        // A strict-mode run that left a hunk unapplied which structural
        // matching would have placed is the trigger for the strict→structural
        // follow-up. `follow_up_id` names the action `register_follow_ups`
        // registers (and the registry surfaces in the model-facing
        // `follow_ups` array); it is null when no escalation applies.
        let follow_up_id = if matches!(args.mode, PatchMode::Strict)
            && has_viable_strict_alternative(&resolution_details)
        {
            serde_json::Value::String("apply_structural".to_string())
        } else {
            serde_json::Value::Null
        };

        if !all_diagnostics.is_empty() && !allow_broken {
            // Gate: do not write any file to disk.
            let payload = serde_json::json!({
                "kind": "patch_blocked_by_ast",
                "message": "apply_patch rejected: staged content has syntax errors",
                "files_modified": Vec::<String>::new(),
                "files_attempted": staged_files
                    .iter()
                    .map(|s| s.path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                "hunks_applied": total_hunks,
                "lines_added": total_added,
                "lines_removed": total_removed,
                "diagnostics": all_diagnostics,
                "resolution_details": resolution_details,
                "check_overrides": check_overrides,
                "committed": false,
                "mode": mode_str,
                "follow_up_id": follow_up_id,
            });
            return Ok(ToolOutput {
                content: payload,
                is_error: true,
                duration: started.elapsed(),
            });
        }

        if !all_diagnostics.is_empty() && allow_broken {
            check_overrides.push(CheckOverride {
                check_name: "ast_validation".to_string(),
                flag: ToolFlag::AllowBrokenAst,
                source: ctx
                    .flag_source(&ToolFlag::AllowBrokenAst)
                    .unwrap_or("")
                    .to_string(),
            });
        }

        // Commit staged changes in two phases:
        //   1. Write Modify and Create files (creating parent dirs for
        //      Create) — these may overwrite or add disk state.
        //   2. Delete Delete files — only after every non-Delete file has
        //      committed successfully, so a write failure never leaves
        //      the patch half-applied with files already removed.
        // On any failure we roll back everything we already touched:
        // Modify reverts to its captured original, Create is unlinked,
        // Delete is restored from captured before-content.
        let mut applied: Vec<usize> = Vec::with_capacity(staged_files.len());

        for (idx, staged) in staged_files.iter().enumerate() {
            if matches!(staged.kind, PatchBlockKind::Delete) {
                continue;
            }
            if matches!(staged.kind, PatchBlockKind::Create)
                && let Some(parent) = staged.path.parent()
                && !parent.as_os_str().is_empty()
                && let Err(e) = tokio::fs::create_dir_all(parent).await
            {
                rollback_applied(&staged_files, &applied).await;
                return Err(ToolError::ExecutionFailed {
                    reason: format!(
                        "failed to create parent directories for {}: {e}",
                        staged.path.display()
                    ),
                });
            }
            if let Err(e) = tokio::fs::write(&staged.path, staged.staged.as_bytes()).await {
                rollback_applied(&staged_files, &applied).await;
                return Err(ToolError::ExecutionFailed {
                    reason: format!("failed to write {}: {e}", staged.path.display()),
                });
            }
            applied.push(idx);
        }

        for (idx, staged) in staged_files.iter().enumerate() {
            if !matches!(staged.kind, PatchBlockKind::Delete) {
                continue;
            }
            if let Err(e) = tokio::fs::remove_file(&staged.path).await {
                rollback_applied(&staged_files, &applied).await;
                return Err(ToolError::ExecutionFailed {
                    reason: format!("failed to delete {}: {e}", staged.path.display()),
                });
            }
            applied.push(idx);
        }

        let payload = serde_json::json!({
            "kind": "patch_committed",
            "files_modified": staged_files
                .iter()
                .filter(|s| !matches!(s.kind, PatchBlockKind::Delete))
                .map(|s| s.path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            "hunks_applied": total_hunks,
            "lines_added": total_added,
            "lines_removed": total_removed,
            "diagnostics": all_diagnostics,
            "resolution_details": resolution_details,
            "check_overrides": check_overrides,
            "committed": true,
            "mode": mode_str,
            "follow_up_id": follow_up_id,
            "per_file": staged_files
                .iter()
                .map(|s| serde_json::json!({
                    "path": s.path.to_string_lossy(),
                    "status": match s.kind {
                        PatchBlockKind::Modify => "modified",
                        PatchBlockKind::Create => "created",
                        PatchBlockKind::Delete => "deleted",
                    },
                    "hunks": s.hunks,
                    "lines_added": s.added,
                    "lines_removed": s.removed,
                }))
                .collect::<Vec<_>>(),
        });

        Ok(ToolOutput {
            content: payload,
            is_error: has_error_diagnostic,
            duration: started.elapsed(),
        })
    }

    async fn post_validate(&self, output: &ToolOutput, _ctx: &ToolContext) -> PostValidateOutcome {
        let errors: Vec<String> = output
            .content
            .get("diagnostics")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|d| {
                        d.get("severity").and_then(serde_json::Value::as_str) == Some("error")
                    })
                    .filter_map(|d| d.get("message").and_then(serde_json::Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        if errors.is_empty() {
            PostValidateOutcome::Pass
        } else {
            PostValidateOutcome::Fail { errors }
        }
    }

    async fn on_success(&self, output: &ToolOutput, ctx: &ToolContext) {
        let Some(files) = output
            .content
            .get("files_modified")
            .and_then(serde_json::Value::as_array)
        else {
            return;
        };
        for f in files {
            if let Some(path_str) = f.as_str() {
                ctx.mark_file_read(Path::new(path_str));
            }
        }
    }

    /// Register the strict→structural escalation follow-up.
    ///
    /// When a strict-mode run leaves a hunk unapplied that structural matching
    /// would have placed, offer an `apply_patch` follow-up that re-runs the
    /// same patch with `mode: auto`. The action only overrides `mode`; the
    /// runtime merges it over the original call's args, so the original patch
    /// text is reused without re-generation. The follow-up expires if any of
    /// the touched/attempted files change before it runs, since that would
    /// invalidate the structural placement the alternative reported.
    async fn register_follow_ups(
        &self,
        output: &ToolOutput,
        _ctx: &ToolContext,
    ) -> Vec<FollowUpAction> {
        if output
            .content
            .get("mode")
            .and_then(serde_json::Value::as_str)
            != Some("strict")
        {
            return Vec::new();
        }
        let Some(details) = output
            .content
            .get("resolution_details")
            .and_then(serde_json::Value::as_array)
        else {
            return Vec::new();
        };
        let viable = details
            .iter()
            .filter(|d| {
                d.get("applied").and_then(serde_json::Value::as_bool) == Some(false)
                    && d.get("structural_alternative")
                        .is_some_and(|v| !v.is_null())
            })
            .count();
        if viable == 0 {
            return Vec::new();
        }

        // Key the expiry on the current on-disk hashes of the files this patch
        // touched or attempted. If any changes before the follow-up runs, the
        // reported structural alternative is stale and the action expires.
        let mut file_hashes: HashMap<PathBuf, String> = HashMap::new();
        for key in ["files_modified", "files_attempted"] {
            let Some(arr) = output
                .content
                .get(key)
                .and_then(serde_json::Value::as_array)
            else {
                continue;
            };
            for v in arr {
                let Some(path_str) = v.as_str() else { continue };
                let path = PathBuf::from(path_str);
                if file_hashes.contains_key(&path) {
                    continue;
                }
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    file_hashes.insert(path, hash_content(&bytes));
                }
            }
        }
        let expires = if file_hashes.is_empty() {
            ExpiryCondition::Never
        } else {
            ExpiryCondition::AnyFileModified { files: file_hashes }
        };

        vec![FollowUpAction {
            action: "apply_structural".to_string(),
            description: format!(
                "Re-apply this patch with structural matching (mode: auto). {viable} hunk(s) failed strict exact-position matching but structural matching would resolve them."
            ),
            tool: "apply_patch".to_string(),
            args: serde_json::json!({ "mode": "auto" }),
            expires,
            confidence: Confidence::High,
            before_content: BeforeContentSource::Unavailable,
        }]
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};
    use crate::tools::patch_entity::ExtractedEntity;
    use crate::tools::patch_parse::{PatchBlockKind, extract_headers};

    /// Mock extractor returning a single fixed entity, used to drive
    /// end-to-end tier-1 resolution through `ApplyPatchTool::with_extractor`.
    struct SingleEntityExtractor {
        entity: ExtractedEntity,
    }

    impl EntityExtractor for SingleEntityExtractor {
        fn extract(&self, _source: &str, _path: &Path) -> Option<Vec<ExtractedEntity>> {
            Some(vec![self.entity.clone()])
        }
    }

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "apply_patch".to_string(),
            model_args: args,
            runtime_inputs: RuntimeInputs::default(),
            metadata: serde_json::Value::Null,
        }
    }

    fn make_patch(target_rel: &str, original: &str, modified: &str) -> String {
        let patch = diffy::create_patch(original, modified).to_string();
        // Replace `--- original` / `+++ modified` with our target path so
        // the parser knows what file to touch.
        let mut lines: Vec<String> = patch.lines().map(str::to_string).collect();
        for line in &mut lines {
            if line.starts_with("--- ") {
                *line = format!("--- a/{target_rel}");
            } else if line.starts_with("+++ ") {
                *line = format!("+++ b/{target_rel}");
            }
        }
        lines.join("\n") + "\n"
    }

    #[tokio::test]
    async fn applies_valid_patch_to_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn main() {\n    let x = 1;\n}\n";
        let modified = "fn main() {\n    let x = 2;\n}\n";
        tokio::fs::write(&path, original).await.unwrap();

        let patch_text = make_patch("file.rs", original, modified);

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(out.content["committed"], true);
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(on_disk, modified);
    }

    #[tokio::test]
    async fn pre_validate_blocks_when_target_not_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn main() {}\n";
        let modified = "fn entry() {}\n";
        tokio::fs::write(&path, original).await.unwrap();

        let patch_text = make_patch("file.rs", original, modified);
        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        // Do NOT mark read.
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        match tool.pre_validate(&env, &ctx).await {
            PreValidateOutcome::Block { reason } => {
                assert!(reason.contains("not read"), "{reason}");
            }
            PreValidateOutcome::Proceed => panic!("expected Block"),
        }
    }

    #[tokio::test]
    async fn bad_context_returns_execution_failed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        tokio::fs::write(&path, "fn main() { let x = 1; }\n")
            .await
            .unwrap();

        let bogus_patch = "--- a/file.rs\n+++ b/file.rs\n@@ -1,1 +1,1 @@\n-this line is not in the file\n+replacement\n";
        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": bogus_patch,
            "working_dir": dir.path().to_string_lossy(),
        }));
        let err = tool.execute(&env, &ctx).await.expect_err("bad context");
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));

        let after = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(after, "fn main() { let x = 1; }\n", "original preserved");
    }

    #[tokio::test]
    async fn ast_failure_gates_and_preserves_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn main() {\n    let x = 1;\n}\n";
        // Patch that introduces a syntax error (drops closing brace).
        let modified = "fn main() {\n    let x = 1;\n\n";
        tokio::fs::write(&path, original).await.unwrap();
        let patch_text = make_patch("file.rs", original, modified);

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(out.is_error, "expected gate failure: {:?}", out.content);
        assert_eq!(out.content["committed"], false);
        let diagnostics = out.content["diagnostics"].as_array().unwrap();
        assert!(!diagnostics.is_empty());
        assert_eq!(diagnostics[0]["severity"], "error");
        // patch retains a `file` field on each diagnostic for routing.
        assert!(diagnostics[0]["file"].is_string());

        let after = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(after, original, "disk content unchanged on AST failure");
    }

    #[tokio::test]
    async fn ast_failure_with_allow_broken_commits_and_records_override() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn main() {\n    let x = 1;\n}\n";
        let modified = "fn main() {\n    let x = 1;\n\n";
        tokio::fs::write(&path, original).await.unwrap();
        let patch_text = make_patch("file.rs", original, modified);

        let tool = ApplyPatchTool::new();
        let mut ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        ctx.set_flag(ToolFlag::AllowBrokenAst, "test:override");
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        // With AllowBrokenAst the patch commits but error-severity
        // diagnostics still mark the output as is_error=true.
        assert!(out.is_error, "{:?}", out.content);
        assert_eq!(out.content["committed"], true);
        let overrides = out.content["check_overrides"].as_array().unwrap();
        assert_eq!(overrides.len(), 1);
    }

    #[tokio::test]
    async fn post_validate_mode_is_gate() {
        let tool = ApplyPatchTool::new();
        assert_eq!(tool.post_validate_mode(), PostValidateMode::Gate);
    }

    #[tokio::test]
    async fn on_success_marks_modified_files_as_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn main() {\n    let x = 1;\n}\n";
        let modified = "fn main() {\n    let x = 2;\n}\n";
        tokio::fs::write(&path, original).await.unwrap();
        let patch_text = make_patch("file.rs", original, modified);

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        tool.on_success(&out, &ctx).await;
        assert!(ctx.has_read_file(&path));
    }

    fn create_patch(target_rel: &str, content_lines: &[&str]) -> String {
        use std::fmt::Write as _;
        let mut s = format!("--- /dev/null\n+++ b/{target_rel}\n");
        let n = content_lines.len();
        let _ = writeln!(s, "@@ -0,0 +1,{n} @@");
        for line in content_lines {
            s.push('+');
            s.push_str(line);
            s.push('\n');
        }
        s
    }

    fn delete_patch(target_rel: &str, original_lines: &[&str]) -> String {
        use std::fmt::Write as _;
        let mut s = format!("--- a/{target_rel}\n+++ /dev/null\n");
        let n = original_lines.len();
        let _ = writeln!(s, "@@ -1,{n} +0,0 @@");
        for line in original_lines {
            s.push('-');
            s.push_str(line);
            s.push('\n');
        }
        s
    }

    #[tokio::test]
    async fn creates_file_from_dev_null() {
        let dir = tempdir().unwrap();
        let target_rel = "new.rs";
        let path = dir.path().join(target_rel);
        let patch_text = create_patch(target_rel, &["fn entry() {}"]);

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        // Pre-validate must not complain about file-exists or read-before-edit.
        match tool.pre_validate(&env, &ctx).await {
            PreValidateOutcome::Proceed => {}
            PreValidateOutcome::Block { reason } => panic!("expected Proceed, got Block: {reason}"),
        }

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(out.content["committed"], true);

        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(on_disk, "fn entry() {}\n");

        let per_file = out.content["per_file"].as_array().unwrap();
        assert_eq!(per_file.len(), 1);
        assert_eq!(per_file[0]["status"], "created");

        let files_modified = out.content["files_modified"].as_array().unwrap();
        assert_eq!(files_modified.len(), 1);
        assert!(files_modified[0].as_str().unwrap().ends_with(target_rel));
    }

    #[tokio::test]
    async fn deletes_file_to_dev_null() {
        let dir = tempdir().unwrap();
        let target_rel = "doomed.rs";
        let path = dir.path().join(target_rel);
        let original = "fn dies() {}\n";
        tokio::fs::write(&path, original).await.unwrap();

        let patch_text = delete_patch(target_rel, &["fn dies() {}"]);

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        match tool.pre_validate(&env, &ctx).await {
            PreValidateOutcome::Proceed => {}
            PreValidateOutcome::Block { reason } => panic!("expected Proceed, got Block: {reason}"),
        }

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(out.content["committed"], true);
        assert!(!path.exists(), "file should be removed from disk");

        let per_file = out.content["per_file"].as_array().unwrap();
        assert_eq!(per_file.len(), 1);
        assert_eq!(per_file[0]["status"], "deleted");

        let files_modified = out.content["files_modified"].as_array().unwrap();
        assert!(
            files_modified.is_empty(),
            "deleted files must not appear in files_modified (C24): {files_modified:?}",
        );

        let diagnostics = out.content["diagnostics"].as_array().unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[tokio::test]
    async fn mixed_create_and_modify() {
        let dir = tempdir().unwrap();
        let modify_rel = "existing.rs";
        let modify_path = dir.path().join(modify_rel);
        let original = "fn old() {}\n";
        let modified = "fn renamed() {}\n";
        tokio::fs::write(&modify_path, original).await.unwrap();

        let create_rel = "fresh.rs";
        let create_path = dir.path().join(create_rel);

        let modify_block = make_patch(modify_rel, original, modified);
        let create_block = create_patch(create_rel, &["fn brand_new() {}"]);
        let patch_text = format!("{modify_block}{create_block}");

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&modify_path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(out.content["committed"], true);

        let modified_on_disk = tokio::fs::read_to_string(&modify_path).await.unwrap();
        assert_eq!(modified_on_disk, modified);
        let created_on_disk = tokio::fs::read_to_string(&create_path).await.unwrap();
        assert_eq!(created_on_disk, "fn brand_new() {}\n");

        let per_file = out.content["per_file"].as_array().unwrap();
        let statuses: Vec<&str> = per_file
            .iter()
            .map(|e| e["status"].as_str().unwrap())
            .collect();
        assert!(statuses.contains(&"modified"));
        assert!(statuses.contains(&"created"));

        // Only the Modify block's hunk yields a resolution entry; the Create
        // block places no hunks against existing content (R5).
        let details = out.content["resolution_details"].as_array().unwrap();
        assert_eq!(details.len(), 1);
        assert!(details[0]["file"].as_str().unwrap().ends_with(modify_rel));
    }

    #[tokio::test]
    async fn mixed_modify_then_delete() {
        let dir = tempdir().unwrap();
        let modify_rel = "kept.rs";
        let modify_path = dir.path().join(modify_rel);
        let original = "fn keeper() {}\n";
        let modified = "fn keeper_renamed() {}\n";
        tokio::fs::write(&modify_path, original).await.unwrap();

        let delete_rel = "doomed.rs";
        let delete_path = dir.path().join(delete_rel);
        tokio::fs::write(&delete_path, "fn dies() {}\n")
            .await
            .unwrap();

        let modify_block = make_patch(modify_rel, original, modified);
        let delete_block = delete_patch(delete_rel, &["fn dies() {}"]);
        let patch_text = format!("{modify_block}{delete_block}");

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&modify_path);
        ctx.mark_file_read(&delete_path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(out.content["committed"], true);

        let modified_on_disk = tokio::fs::read_to_string(&modify_path).await.unwrap();
        assert_eq!(modified_on_disk, modified);
        assert!(!delete_path.exists());

        let files_modified = out.content["files_modified"].as_array().unwrap();
        assert_eq!(
            files_modified.len(),
            1,
            "only the modified file, not the deleted one"
        );
        assert!(
            files_modified[0].as_str().unwrap().ends_with(modify_rel),
            "files_modified should contain the modified file: {files_modified:?}",
        );
    }

    #[tokio::test]
    async fn delete_then_modify_rejected() {
        let dir = tempdir().unwrap();
        let target_rel = "foo.rs";
        let path = dir.path().join(target_rel);
        let original = "fn foo() {}\n";
        tokio::fs::write(&path, original).await.unwrap();

        let delete_block = delete_patch(target_rel, &["fn foo() {}"]);
        // Modify block contents are irrelevant — pre_validate rejects
        // before the patch text is applied.
        let modify_block = make_patch(target_rel, original, "fn renamed() {}\n");
        let patch_text = format!("{delete_block}{modify_block}");

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        match tool.pre_validate(&env, &ctx).await {
            PreValidateOutcome::Block { reason } => {
                assert!(
                    reason.contains("deleted by block") && reason.contains("modified by block"),
                    "{reason}"
                );
                assert!(reason.contains(target_rel), "{reason}");
            }
            PreValidateOutcome::Proceed => panic!("expected Block"),
        }
    }

    #[tokio::test]
    async fn creates_nested_parent_directories() {
        let dir = tempdir().unwrap();
        let nested_rel = "deep/nested/path/new_mod.rs";
        let nested_path = dir.path().join(nested_rel);
        let patch_text = create_patch(nested_rel, &["fn nested() {}"]);

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(out.content["committed"], true);
        assert!(nested_path.exists());
        let on_disk = tokio::fs::read_to_string(&nested_path).await.unwrap();
        assert_eq!(on_disk, "fn nested() {}\n");
    }

    #[tokio::test]
    async fn creation_with_invalid_syntax_rejected_by_ast() {
        let dir = tempdir().unwrap();
        let target_rel = "broken.rs";
        let path = dir.path().join(target_rel);
        // Missing closing brace — tree-sitter Rust grammar will flag.
        let patch_text = create_patch(target_rel, &["fn broken() {"]);

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(out.is_error, "expected gate failure: {:?}", out.content);
        assert_eq!(out.content["committed"], false);
        let diagnostics = out.content["diagnostics"].as_array().unwrap();
        assert!(!diagnostics.is_empty());
        assert!(
            !path.exists(),
            "file must not be created when AST gate fires"
        );
    }

    #[tokio::test]
    async fn create_target_already_existing_is_rejected() {
        let dir = tempdir().unwrap();
        let target_rel = "preexisting.rs";
        let path = dir.path().join(target_rel);
        tokio::fs::write(&path, "fn already() {}\n").await.unwrap();

        let patch_text = create_patch(target_rel, &["fn new() {}"]);

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        match tool.pre_validate(&env, &ctx).await {
            PreValidateOutcome::Block { reason } => {
                assert!(reason.contains("already exists"), "{reason}");
            }
            PreValidateOutcome::Proceed => panic!("expected Block"),
        }
    }

    #[test]
    fn extract_headers_detects_create_with_git_prefix() {
        let block = "--- a/dev/null\n+++ b/src/new.rs\n@@ -0,0 +1,1 @@\n+x\n";
        let (target, kind) = extract_headers(block).unwrap();
        assert_eq!(target, "src/new.rs");
        assert_eq!(kind, PatchBlockKind::Create);
    }

    #[test]
    fn extract_headers_detects_create_with_literal_dev_null() {
        let block = "--- /dev/null\n+++ b/src/new.rs\n@@ -0,0 +1,1 @@\n+x\n";
        let (target, kind) = extract_headers(block).unwrap();
        assert_eq!(target, "src/new.rs");
        assert_eq!(kind, PatchBlockKind::Create);
    }

    #[test]
    fn extract_headers_detects_delete() {
        let block = "--- a/src/old.rs\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-x\n";
        let (target, kind) = extract_headers(block).unwrap();
        assert_eq!(target, "src/old.rs");
        assert_eq!(kind, PatchBlockKind::Delete);
    }

    #[test]
    fn extract_headers_detects_modify() {
        let block = "--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -1,1 +1,1 @@\n-x\n+y\n";
        let (target, kind) = extract_headers(block).unwrap();
        assert_eq!(target, "src/foo.rs");
        assert_eq!(kind, PatchBlockKind::Modify);
    }

    #[tokio::test]
    async fn resolution_details_reports_tier_for_modify() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn other() {\n    let a = 0;\n}\n\nfn target() {\n    let value = 1;\n}\n";
        tokio::fs::write(&path, original).await.unwrap();

        // Intentionally stale line numbers but a correct `fn target` anchor.
        // The tool is built with an extractor that reports `target` at lines
        // 5-7, so resolution must land at tier 1 and surface in
        // resolution_details.
        let patch_text = "\
--- a/file.rs
+++ b/file.rs
@@ -42,3 +42,3 @@ fn target
 fn target() {
-    let value = 1;
+    let value = 2;
 }
";

        let extractor = Arc::new(SingleEntityExtractor {
            entity: ExtractedEntity {
                name: Some("target".to_string()),
                qualified_name: "target".to_string(),
                kind: "fn".to_string(),
                byte_range: 0..0,
                // 1-indexed inclusive: `fn target` spans lines 5-7.
                line_range: 5..7,
            },
        });
        let tool = ApplyPatchTool::with_extractor(extractor);
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(out.content["committed"], true);

        let details = out.content["resolution_details"].as_array().unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0]["tier_used"], 1);
        assert_eq!(details[0]["entity_matched"], "fn target");
        assert_eq!(details[0]["confidence"], "High");
        assert_eq!(details[0]["stated_line"], 42);
        assert_eq!(details[0]["hunk_index"], 1);
        assert!(details[0]["file"].as_str().unwrap().ends_with("file.rs"));

        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(on_disk.contains("let value = 2;"));
    }

    #[tokio::test]
    async fn resolution_details_skip_tier_1_without_extractor() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn other() {\n    let a = 0;\n}\n\nfn target() {\n    let value = 1;\n}\n";
        tokio::fs::write(&path, original).await.unwrap();

        // Same anchored patch as the tier-1 test, but the default tool carries
        // no extractor: tier 1 is skipped and the hunk resolves at tier 2 via
        // full-file context search.
        let patch_text = "\
--- a/file.rs
+++ b/file.rs
@@ -42,3 +42,3 @@ fn target
 fn target() {
-    let value = 1;
+    let value = 2;
 }
";

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(out.content["committed"], true);

        let details = out.content["resolution_details"].as_array().unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0]["tier_used"], 2);
        assert!(details[0]["entity_matched"].is_null());

        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(on_disk.contains("let value = 2;"));
    }

    // --- NTP-005 R1/R7: mode parameter schema & defaulting ---------------

    #[test]
    fn patch_args_schema_matches_previous_hand_written_schema() {
        let expected_schema = json!({
            "type": "object",
            "required": ["patch"],
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "Unified-diff patch text. Supports single-file and multi-file patches."
                },
                "working_dir": {
                    "type": "string",
                    "description": "Directory to resolve relative paths in the patch against."
                },
                "mode": {
                    "type": "string",
                    "enum": ["auto", "strict", "structural"],
                    "default": "auto",
                    "description": "Hunk-resolution mode. 'auto' (default) resolves entity-first: entity-guided placement, then context search, then header-corrected diffy. 'strict' applies a hunk only when its context matches exactly at the stated @@ line; failures are non-fatal and report whether structural matching would have succeeded — use it to verify your line numbers. 'structural' requires a semantic anchor and entity resolution for every hunk, searches context only within the entity's range, and never falls back to diffy."
                }
            },
            "additionalProperties": false
        });
        assert_eq!(PatchArgs::json_schema(), expected_schema);
    }

    #[test]
    fn input_schema_exposes_mode_enum_and_default() {
        let tool = ApplyPatchTool::new();
        let schema = tool.input_schema();
        let mode = &schema["properties"]["mode"];
        assert_eq!(mode["type"], "string");
        assert_eq!(mode["default"], "auto");
        let vals: Vec<&str> = mode["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(vals, vec!["auto", "strict", "structural"]);
        assert!(mode["description"].as_str().unwrap().contains("strict"));
    }

    #[test]
    fn patch_mode_defaults_to_auto_when_omitted() {
        assert_eq!(PatchMode::default(), PatchMode::Auto);
        let args: PatchArgs = serde_json::from_value(json!({ "patch": "x" })).unwrap();
        assert_eq!(args.mode, PatchMode::Auto);

        let strict: PatchArgs =
            serde_json::from_value(json!({ "patch": "x", "mode": "strict" })).unwrap();
        assert_eq!(strict.mode, PatchMode::Strict);
        let structural: PatchArgs =
            serde_json::from_value(json!({ "patch": "x", "mode": "structural" })).unwrap();
        assert_eq!(structural.mode, PatchMode::Structural);
    }

    /// Patch text whose `@@` header states line 1 but whose context lives at
    /// line 2 (after a leading comment). Strict fails; structural would match.
    fn drifted_patch() -> &'static str {
        "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
"
    }

    #[tokio::test]
    async fn strict_failure_emits_structural_alternative_and_follow_up_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "// leading comment\nfn main() {\n    let x = 1;\n}\n";
        tokio::fs::write(&path, original).await.unwrap();

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": drifted_patch(),
            "working_dir": dir.path().to_string_lossy(),
            "mode": "strict",
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(out.content["committed"], true);
        assert_eq!(out.content["mode"], "strict");
        assert_eq!(out.content["follow_up_id"], "apply_structural");

        let details = out.content["resolution_details"].as_array().unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0]["applied"], false);
        assert_eq!(details[0]["tier_used"], 0);
        let alt = &details[0]["structural_alternative"];
        assert_eq!(alt["would_apply"], true);
        assert_eq!(alt["matched_at_line"], 2);
        assert_eq!(alt["drift"], 1);

        // The strict failure left the file untouched.
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(on_disk, original);

        // The tool registers a real strict→structural follow-up.
        let follow_ups = tool.register_follow_ups(&out, &ctx).await;
        assert_eq!(follow_ups.len(), 1);
        assert_eq!(follow_ups[0].action, "apply_structural");
        assert_eq!(follow_ups[0].tool, "apply_patch");
        assert_eq!(follow_ups[0].args, json!({ "mode": "auto" }));
    }

    #[tokio::test]
    async fn strict_success_has_no_follow_up() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn main() {\n    let x = 1;\n}\n";
        tokio::fs::write(&path, original).await.unwrap();

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": drifted_patch(),
            "working_dir": dir.path().to_string_lossy(),
            "mode": "strict",
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert_eq!(out.content["committed"], true);
        let details = out.content["resolution_details"].as_array().unwrap();
        assert_eq!(details[0]["applied"], true);
        assert_eq!(details[0]["drift"], 0);
        assert!(out.content["follow_up_id"].is_null());

        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(on_disk.contains("let x = 2;"));

        assert!(tool.register_follow_ups(&out, &ctx).await.is_empty());
    }

    #[tokio::test]
    async fn registry_dispatches_apply_patch() {
        use std::sync::Arc;

        use crate::r#loop::runner::ToolExecutor;
        use crate::tool::registry::ToolRegistry;

        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "// leading comment\nfn main() {\n    let x = 1;\n}\n";
        tokio::fs::write(&path, original).await.unwrap();

        let ctx = Arc::new(ToolContext::empty());
        ctx.mark_file_read(&path);
        let mut reg = ToolRegistry::with_context(Arc::clone(&ctx));
        reg.register(Box::new(ApplyPatchTool::new()));
        let executor: &dyn ToolExecutor = &reg;

        let out = executor
            .execute(
                "apply_patch",
                "test-call",
                json!({
                    "patch": drifted_patch(),
                    "working_dir": dir.path().to_string_lossy(),
                    "mode": "strict",
                }),
            )
            .await
            .expect("dispatch succeeds");

        // Strict mode with drift should report that context did not match.
        assert!(out.is_object(), "expected JSON object output");
    }
}
