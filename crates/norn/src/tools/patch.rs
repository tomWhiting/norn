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

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::ast::{AstCheck, check_syntax};
use super::confinement::check_confinement;
use super::patch_commit::commit_staged;
use super::patch_entity::EntityExtractor;
use super::patch_followup::{has_viable_strict_alternative, strict_escalation_follow_ups};
use super::patch_gate::{effective_working_dir, pre_validate_patch};
use super::patch_parse::{PatchBlockKind, parse_blocks};
use super::patch_stage::stage_blocks;
use super::write::syntax_error_to_diagnostic;
use crate::error::ToolError;
use crate::tool::ToolArgs;
use crate::tool::context::{ToolContext, ToolFlag};
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::follow_up::FollowUpAction;
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
pub(super) struct PatchArgs {
    /// Unified-diff patch text. Supports single-file and multi-file patches.
    pub(super) patch: String,
    /// Directory to resolve relative paths in the patch against.
    #[serde(default)]
    pub(super) working_dir: Option<String>,
    /// Hunk-resolution mode. Defaults to [`PatchMode::Auto`] when omitted.
    #[serde(default)]
    #[tool_args(schema = {
        "type": "string",
        "enum": ["auto", "strict", "structural"],
        "default": "auto",
        "description": "Hunk-resolution mode. 'auto' (default) resolves entity-first: entity-guided placement, then context search, then header-corrected diffy. 'strict' applies a hunk only when its context matches exactly at the stated @@ line; failures are non-fatal and report whether structural matching would have succeeded — use it to verify your line numbers. 'structural' requires a semantic anchor and entity resolution for every hunk, searches context only within the entity's range, and never falls back to diffy."
    })]
    pub(super) mode: PatchMode,
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
        pre_validate_patch(envelope, ctx)
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: PatchArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("invalid arguments: {e}"),
            }
        })?;

        let blocks = parse_blocks(&args.patch).map_err(|e| ToolError::ExecutionFailed {
            reason: format!("patch parse failed: {e}"),
        })?;

        let effective_wd = effective_working_dir(args.working_dir.as_deref(), ctx);
        if args.working_dir.is_some()
            && let Err(reason) = check_confinement(ctx, &effective_wd)
        {
            return Err(ToolError::ExecutionFailed {
                reason: format!("apply_patch: working_dir refused: {reason}"),
            });
        }

        let staged_set = stage_blocks(
            blocks,
            &effective_wd,
            ctx,
            self.extractor.as_deref(),
            args.mode,
        )
        .await?;
        let staged_files = &staged_set.files;
        let total_hunks = staged_set.total_hunks;
        let total_added = staged_set.total_added;
        let total_removed = staged_set.total_removed;
        let resolution_details = staged_set.resolution_details;

        // AST-validate each file's *final* staged content (not per-block
        // intermediates, which may legitimately be transitional states when
        // several blocks touch the same file). Deletions have no
        // post-mutation content to validate.
        let mut all_diagnostics: Vec<serde_json::Value> = Vec::new();
        for staged in staged_files {
            if matches!(staged.kind, PatchBlockKind::Delete) {
                continue;
            }
            let ast = check_syntax(&staged.path, &staged.staged);
            if let AstCheck::Fail { errors } = &ast {
                for e in errors {
                    let mut diag = syntax_error_to_diagnostic(e);
                    if let Some(map) = diag.as_object_mut() {
                        map.insert(
                            "file".to_string(),
                            serde_json::Value::String(staged.path.to_string_lossy().into_owned()),
                        );
                    }
                    all_diagnostics.push(diag);
                }
            }
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
                "diagnostics": all_diagnostics.clone(),
                "resolution_details": resolution_details,
                "check_overrides": check_overrides,
                "committed": false,
                "mode": mode_str,
                "follow_up_id": follow_up_id,
            });
            return Ok(ToolOutput::failure_with_content(
                payload,
                ToolErrorPayload::new(
                    ToolErrorKind::ValidationFailed,
                    "apply_patch rejected: staged content has syntax errors",
                )
                .with_detail(serde_json::json!({ "diagnostics": all_diagnostics })),
            ));
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

        // Commit staged changes atomically (temp file + rename per file,
        // two-phase write-then-delete ordering, full rollback on failure).
        commit_staged(staged_files).await?;

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
            "diagnostics": all_diagnostics.clone(),
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

        if has_error_diagnostic {
            return Ok(ToolOutput::failure_with_content(
                payload,
                ToolErrorPayload::new(
                    ToolErrorKind::ValidationFailed,
                    "patch committed with syntax errors (AllowBrokenAst override)",
                )
                .with_detail(serde_json::json!({ "diagnostics": all_diagnostics })),
            ));
        }
        Ok(ToolOutput::success(payload))
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

    /// Register the strict→structural escalation follow-up (see
    /// `patch_followup::strict_escalation_follow_ups`).
    async fn register_follow_ups(
        &self,
        output: &ToolOutput,
        _ctx: &ToolContext,
    ) -> Vec<FollowUpAction> {
        strict_escalation_follow_ups(output).await
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
        assert!(!out.is_error(), "{:?}", out.content);
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
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => {
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
        assert!(out.is_error(), "expected gate failure: {:?}", out.content);
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
        assert!(out.is_error(), "{:?}", out.content);
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
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => panic!("expected Proceed, got Block: {reason}"),
        }

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
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
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => panic!("expected Proceed, got Block: {reason}"),
        }

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
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
        assert!(!out.is_error(), "{:?}", out.content);
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
        assert!(!out.is_error(), "{:?}", out.content);
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
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => {
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
        assert!(!out.is_error(), "{:?}", out.content);
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
        assert!(out.is_error(), "expected gate failure: {:?}", out.content);
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
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => {
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
        assert!(!out.is_error(), "{:?}", out.content);
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
        assert!(!out.is_error(), "{:?}", out.content);
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
        assert!(!out.is_error(), "{:?}", out.content);
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

    // --- H8 regression: multiple blocks on the same file -------------------

    #[tokio::test]
    async fn two_blocks_on_same_file_both_land() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn alpha() {\n    let a = 1;\n}\n\nfn beta() {\n    let b = 1;\n}\n";
        tokio::fs::write(&path, original).await.unwrap();

        // The reproduced scenario: one block per change, both targeting the
        // same file. The historical bug staged each block from the disk
        // original, so the second write silently discarded the first change.
        let patch_text = "\
--- a/file.rs
+++ b/file.rs
@@ -2,1 +2,1 @@
-    let a = 1;
+    let a = 2;
--- a/file.rs
+++ b/file.rs
@@ -6,1 +6,1 @@
-    let b = 1;
+    let b = 2;
";

        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["committed"], true);

        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(
            on_disk.contains("let a = 2;"),
            "first block landed: {on_disk}"
        );
        assert!(
            on_disk.contains("let b = 2;"),
            "second block landed: {on_disk}"
        );

        // The two blocks merge into one per_file entry with both hunks.
        let per_file = out.content["per_file"].as_array().unwrap();
        assert_eq!(per_file.len(), 1);
        assert_eq!(per_file[0]["hunks"], 2);
        assert_eq!(out.content["files_modified"].as_array().unwrap().len(), 1);
    }

    // --- EOL preservation regression ----------------------------------------

    #[tokio::test]
    async fn crlf_file_stays_crlf_byte_for_byte_outside_the_hunk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let original = "alpha\r\nbeta\r\ngamma\r\n";
        tokio::fs::write(&path, original).await.unwrap();

        let patch_text = "\
--- a/file.txt
+++ b/file.txt
@@ -2,1 +2,1 @@
-beta
+BETA
";
        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        let on_disk = tokio::fs::read(&path).await.unwrap();
        assert_eq!(
            String::from_utf8(on_disk).unwrap(),
            "alpha\r\nBETA\r\ngamma\r\n",
            "CRLF endings preserved byte-for-byte"
        );
    }

    #[tokio::test]
    async fn missing_trailing_newline_is_preserved() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let original = "alpha\nbeta";
        tokio::fs::write(&path, original).await.unwrap();

        let patch_text = "\
--- a/file.txt
+++ b/file.txt
@@ -1,1 +1,1 @@
-alpha
+ALPHA
";
        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(on_disk, "ALPHA\nbeta", "no trailing newline appended");
    }

    #[tokio::test]
    async fn git_diff_with_no_newline_markers_round_trips() {
        // A standard git diff against a file with no trailing newline, where
        // the new side also lacks one: both sides carry the marker.
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        tokio::fs::write(&path, "alpha\nbeta").await.unwrap();

        let patch_text = "\
--- a/file.txt
+++ b/file.txt
@@ -1,2 +1,2 @@
 alpha
-beta
\\ No newline at end of file
+BETA
\\ No newline at end of file
";
        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(on_disk, "alpha\nBETA", "no trailing newline on the result");
    }

    #[tokio::test]
    async fn marker_on_old_side_only_adds_trailing_newline() {
        // The old file lacks a trailing newline (marker after the `-` line);
        // the new side carries no marker, so the patched file gains one.
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        tokio::fs::write(&path, "alpha\nbeta").await.unwrap();

        let patch_text = "\
--- a/file.txt
+++ b/file.txt
@@ -2,1 +2,2 @@
-beta
\\ No newline at end of file
+beta
+gamma
";
        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(
            on_disk, "alpha\nbeta\ngamma\n",
            "new side has no marker, so the result gains a trailing newline"
        );
    }

    #[tokio::test]
    async fn marker_on_new_side_only_removes_trailing_newline() {
        // The old file ends with a newline; the new side's final line carries
        // the marker, so the patched file must lose its trailing newline.
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        tokio::fs::write(&path, "alpha\nbeta\n").await.unwrap();

        let patch_text = "\
--- a/file.txt
+++ b/file.txt
@@ -2,1 +2,1 @@
-beta
+BETA
\\ No newline at end of file
";
        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(on_disk, "alpha\nBETA", "trailing newline removed");
    }

    // --- Block-splitter regression: `-- `-prefixed removals ------------------

    #[tokio::test]
    async fn diff_removing_sql_comment_lines_parses_and_applies() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("query.sql");
        let original = "SELECT 1;\n-- legacy comment\nSELECT 2;\n";
        tokio::fs::write(&path, original).await.unwrap();

        // Removing `-- legacy comment` renders as `--- legacy comment`,
        // which the old splitter misparsed as a new file header.
        let patch_text = "\
--- a/query.sql
+++ b/query.sql
@@ -1,3 +1,2 @@
 SELECT 1;
--- legacy comment
 SELECT 2;
";
        let tool = ApplyPatchTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": dir.path().to_string_lossy(),
        }));

        match tool.pre_validate(&env, &ctx).await {
            PreValidateOutcome::Proceed => {}
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => panic!("expected Proceed, got Block: {reason}"),
        }
        let out = tool.execute(&env, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(on_disk, "SELECT 1;\nSELECT 2;\n");
    }

    // --- Workspace confinement ------------------------------------------------

    #[tokio::test]
    async fn confined_context_refuses_working_dir_outside_root() {
        let outer = tempdir().unwrap();
        let root = outer.path().join("ws");
        tokio::fs::create_dir(&root).await.unwrap();
        let escape_target = outer.path().join("file.rs");
        let original = "fn main() {\n    let x = 1;\n}\n";
        tokio::fs::write(&escape_target, original).await.unwrap();

        let patch_text = make_patch("file.rs", original, "fn main() {\n    let x = 2;\n}\n");
        let tool = ApplyPatchTool::new();
        let mut ctx = ToolContext::empty();
        ctx.confine_to_workspace(root.clone());
        ctx.set_working_dir(root.clone());
        ctx.mark_file_read(&escape_target);
        // Model supplies a working_dir outside the confinement root.
        let env = envelope_for(json!({
            "patch": patch_text,
            "working_dir": outer.path().to_string_lossy(),
        }));

        match tool.pre_validate(&env, &ctx).await {
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => {
                assert!(reason.contains("working_dir refused"), "{reason}");
            }
            PreValidateOutcome::Proceed => panic!("expected Block"),
        }
        let err = tool.execute(&env, &ctx).await.expect_err("refused");
        assert!(err.to_string().contains("working_dir refused"), "{err}");
        let untouched = tokio::fs::read_to_string(&escape_target).await.unwrap();
        assert_eq!(untouched, original);
    }

    #[tokio::test]
    async fn confined_context_refuses_target_escaping_via_dot_dot() {
        let outer = tempdir().unwrap();
        let root = outer.path().join("ws");
        tokio::fs::create_dir(&root).await.unwrap();
        let escape_target = outer.path().join("secret.rs");
        let original = "fn main() {}\n";
        tokio::fs::write(&escape_target, original).await.unwrap();

        let patch_text = make_patch("../secret.rs", original, "fn renamed() {}\n");
        let tool = ApplyPatchTool::new();
        let mut ctx = ToolContext::empty();
        ctx.confine_to_workspace(root.clone());
        ctx.set_working_dir(root.clone());
        ctx.mark_file_read(&escape_target);
        let env = envelope_for(json!({ "patch": patch_text }));

        match tool.pre_validate(&env, &ctx).await {
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => {
                assert!(reason.contains("target refused"), "{reason}");
            }
            PreValidateOutcome::Proceed => panic!("expected Block"),
        }
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
