//! Edit tool — read-before-edit gate, exact-string replacement,
//! tree-sitter AST validation with gate semantics, and blast-radius
//! reporting.
//!
//! Edit follows gate semantics: the replacement is staged in memory,
//! the staged content is parsed with tree-sitter, and the file on disk
//! is only updated when the parse succeeds. The orchestrator's
//! `AllowBrokenAst` flag downgrades gate to report semantics — the
//! file is written even on AST failure and a `CheckOverride` is
//! recorded in the tool output.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::ast::{AstCheck, check_syntax, containing_symbols};
use super::confinement::check_confinement;
use super::file_commit::commit_file_atomic;
use super::write::syntax_error_to_diagnostic;
use crate::error::ToolError;
use crate::session::action_log::hash_content;
use crate::tool::ToolArgs;
use crate::tool::context::{ToolContext, ToolFlag};
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::follow_up::{BeforeContentSource, Confidence, ExpiryCondition, FollowUpAction};
use crate::tool::lifecycle::{
    BlockDecision, CheckOverride, PostValidateMode, PostValidateOutcome, PreValidateOutcome,
};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Edits an existing file by replacing a single exact match of
/// `old_string` with `new_string`. Validates AST after the staged edit
/// and reports the blast radius in the tool output.
pub struct EditTool;

impl EditTool {
    /// Constructs a stateless Edit tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for EditTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize, ToolArgs)]
struct EditArgs {
    /// Absolute path to the file to edit.
    path: String,
    /// Exact text to find and replace. Must match exactly once.
    old_string: String,
    /// Replacement text.
    new_string: String,
    /// 1-based index selecting which occurrence to replace when `old_string`
    /// matches more than once. Omitted for unambiguous edits; supplied by the
    /// `apply_at_occurrence_N` follow-up to commit a chosen match.
    #[serde(default)]
    #[tool_args(schema = {
        "type": "integer",
        "minimum": 1,
        "description": "1-based occurrence to replace when old_string matches more than once. Omit for a unique match."
    })]
    occurrence: Option<usize>,
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        include_str!("guidance/edit.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::FileSystem
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("guidance/edit.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        EditArgs::json_schema()
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Write
    }

    fn post_validate_mode(&self) -> PostValidateMode {
        PostValidateMode::Gate
    }

    async fn pre_validate(&self, envelope: &ToolEnvelope, ctx: &ToolContext) -> PreValidateOutcome {
        let args: EditArgs = match serde_json::from_value(envelope.model_args.clone()) {
            Ok(args) => args,
            Err(e) => {
                return PreValidateOutcome::Block(
                    BlockDecision::new(format!("invalid arguments: {e}"))
                        .with_kind(ToolErrorKind::InvalidArguments),
                );
            }
        };

        let path = ctx.resolve_path(&args.path);

        if let Err(reason) = check_confinement(ctx, &path) {
            return PreValidateOutcome::Block(
                BlockDecision::new(format!("Edit blocked: {reason}"))
                    .with_kind(ToolErrorKind::PermissionDenied)
                    .with_detail(serde_json::json!({ "path": args.path })),
            );
        }

        if !ctx.has_read_file(&path) {
            return PreValidateOutcome::Block(
                BlockDecision::new("Edit blocked: file has not been read this session")
                    .with_guidance("Read the file with the read tool before editing it.")
                    .with_detail(serde_json::json!({ "path": args.path })),
            );
        }

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                return PreValidateOutcome::Block(
                    BlockDecision::new(format!("Edit blocked: failed to read {}: {e}", args.path))
                        .with_kind(ToolErrorKind::Io)
                        .with_detail(serde_json::json!({ "path": args.path })),
                );
            }
        };

        let count = content.matches(&args.old_string).count();
        if count == 0 {
            return PreValidateOutcome::Block(
                BlockDecision::new("old_string not found in file")
                    .with_kind(ToolErrorKind::NotFound)
                    .with_guidance(
                        "Re-read the file and supply old_string exactly as it appears on disk.",
                    ),
            );
        }
        // An ambiguous match (count > 1) is NOT blocked here: execute surfaces
        // it as a non-committing structured result so register_follow_ups can
        // offer apply_at_occurrence_N actions. A supplied `occurrence` selects
        // which match to commit.
        if count > 1 && args.occurrence.is_none() {
            return PreValidateOutcome::Proceed;
        }
        if let Some(n) = args.occurrence
            && (n == 0 || n > count)
        {
            return PreValidateOutcome::Block(
                BlockDecision::new(format!(
                    "occurrence {n} out of range: old_string matches {count} time(s)"
                ))
                .with_kind(ToolErrorKind::InvalidArguments)
                .with_detail(serde_json::json!({ "occurrence": n, "match_count": count })),
            );
        }

        PreValidateOutcome::Proceed
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: EditArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("invalid arguments: {e}"),
            }
        })?;
        let path = ctx.resolve_path(&args.path);

        // Workspace confinement (opt-in): re-checked at execute time so a
        // direct invocation cannot bypass the pre_validate gate.
        if let Err(reason) = check_confinement(ctx, &path) {
            return Err(ToolError::ExecutionFailed {
                reason: format!("edit refused: {reason}"),
            });
        }

        let original =
            tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("failed to read {} for edit: {e}", args.path),
                })?;

        let match_offsets = occurrence_offsets(&original, &args.old_string);
        let count = match_offsets.len();
        if count == 0 {
            return Ok(ToolOutput::failure_with_content(
                serde_json::json!({
                    "path": args.path,
                    "kind": "edit_failed",
                    "committed": false,
                }),
                ToolErrorPayload::new(ToolErrorKind::NotFound, "old_string not found in file")
                    .with_detail(serde_json::json!({ "path": args.path })),
            ));
        }

        // Resolve which occurrence to apply. An explicit `occurrence` selects a
        // 1-based match (and is validated here so a stale follow-up index fails
        // loudly). Without one, a unique match applies directly while an
        // ambiguous match returns a non-committing result that drives the
        // apply_at_occurrence_N follow-ups.
        let match_byte = if let Some(n) = args.occurrence {
            let Some(&byte) = n.checked_sub(1).and_then(|i| match_offsets.get(i)) else {
                return Ok(ToolOutput::failure_with_content(
                    serde_json::json!({
                        "path": args.path,
                        "kind": "edit_failed",
                        "committed": false,
                    }),
                    ToolErrorPayload::new(
                        ToolErrorKind::InvalidArguments,
                        format!("occurrence {n} out of range: old_string matches {count} time(s)"),
                    )
                    .with_detail(serde_json::json!({ "occurrence": n, "match_count": count })),
                ));
            };
            byte
        } else {
            if count > 1 {
                let occurrences = occurrence_descriptors(&original, &match_offsets);
                return Ok(ToolOutput::failure_with_content(
                    serde_json::json!({
                        "path": args.path,
                        "kind": "edit_ambiguous",
                        "match_count": count,
                        "occurrences": occurrences,
                        "file_hash": hash_content(original.as_bytes()),
                        "committed": false,
                    }),
                    ToolErrorPayload::new(
                        ToolErrorKind::Conflict,
                        format!("old_string matches {count} times; choose an occurrence"),
                    )
                    .with_detail(serde_json::json!({ "match_count": count })),
                ));
            }
            match_offsets[0]
        };

        // Stage the edit in memory. `old_string` was located via byte scan, so
        // the subtraction never underflows.
        let staged_capacity = original
            .len()
            .saturating_sub(args.old_string.len())
            .saturating_add(args.new_string.len());
        let mut staged = String::with_capacity(staged_capacity);
        staged.push_str(&original[..match_byte]);
        staged.push_str(&args.new_string);
        staged.push_str(&original[match_byte + args.old_string.len()..]);

        // Validate staged content.
        let ast_check = check_syntax(&path, &staged);
        let ast_diagnostics: Vec<serde_json::Value> = match &ast_check {
            AstCheck::Pass | AstCheck::Unsupported => Vec::new(),
            AstCheck::Fail { errors } => errors.iter().map(syntax_error_to_diagnostic).collect(),
        };

        let allow_broken = ctx.has_flag(&ToolFlag::AllowBrokenAst);
        let mut check_overrides: Vec<CheckOverride> = Vec::new();
        // R8 deviation: blast radius computed here instead of on_success
        // because on_success returns () and takes &ToolContext, so it
        // cannot enrich ToolOutput.
        let blast = compute_blast_radius(
            &path,
            &original,
            &staged,
            &args.old_string,
            &args.new_string,
            match_byte,
        );

        if !ast_diagnostics.is_empty() && !allow_broken {
            // Gate: do not write to disk, original preserved.
            let payload = serde_json::json!({
                "path": args.path,
                "kind": "edit_blocked_by_ast",
                "diagnostics": ast_diagnostics.clone(),
                "blast_radius": blast,
                "check_overrides": check_overrides,
                "committed": false,
            });
            return Ok(ToolOutput::failure_with_content(
                payload,
                ToolErrorPayload::new(
                    ToolErrorKind::ValidationFailed,
                    "edit rejected: staged content has syntax errors",
                )
                .with_detail(serde_json::json!({ "diagnostics": ast_diagnostics })),
            ));
        }

        if !ast_diagnostics.is_empty() && allow_broken {
            check_overrides.push(CheckOverride {
                check_name: "ast_validation".to_string(),
                flag: ToolFlag::AllowBrokenAst,
                source: ctx
                    .flag_source(&ToolFlag::AllowBrokenAst)
                    .unwrap_or("")
                    .to_string(),
            });
        }

        // Commit to disk: AST passed, or AllowBrokenAst is set. The commit
        // is atomic (temp file in the same directory + rename, preserving
        // the original's permissions) so a crash or ENOSPC mid-write never
        // destroys the original content.
        commit_file_atomic(&path, staged.as_bytes())
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("failed to write edited content to {}: {e}", args.path),
            })?;

        let after_hash = hash_content(staged.as_bytes());

        let has_error_diagnostic = ast_diagnostics
            .iter()
            .any(|d| d.get("severity").and_then(serde_json::Value::as_str) == Some("error"));
        let payload = serde_json::json!({
            "path": args.path,
            "kind": "edit_committed",
            "diagnostics": ast_diagnostics.clone(),
            "blast_radius": blast,
            "check_overrides": check_overrides,
            "committed": true,
            "match_count": count,
            "after_hash": after_hash,
        });

        if has_error_diagnostic {
            return Ok(ToolOutput::failure_with_content(
                payload,
                ToolErrorPayload::new(
                    ToolErrorKind::ValidationFailed,
                    "edit committed with syntax errors (AllowBrokenAst override)",
                )
                .with_detail(serde_json::json!({ "diagnostics": ast_diagnostics })),
            ));
        }
        Ok(ToolOutput::success(payload))
    }

    async fn post_validate(&self, output: &ToolOutput, _ctx: &ToolContext) -> PostValidateOutcome {
        // Mirror the AST result recorded during execute. The disk
        // gating already happened inside execute; this is for runtime
        // introspection layers that read the structured output.
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

    /// Register disambiguation follow-ups.
    ///
    /// An ambiguous, non-committing edit yields one `apply_at_occurrence_N`
    /// action per match, each overriding `occurrence` so the model can commit
    /// the intended location. Zero-match and uncommitted outcomes register
    /// nothing.
    async fn register_follow_ups(
        &self,
        output: &ToolOutput,
        _ctx: &ToolContext,
    ) -> Vec<FollowUpAction> {
        let match_count = output
            .content
            .get("match_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if match_count <= 1 {
            return Vec::new();
        }
        let Some(occurrences) = output
            .content
            .get("occurrences")
            .and_then(serde_json::Value::as_array)
        else {
            return Vec::new();
        };
        let path = PathBuf::from(
            output
                .content
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
        );
        let file_hash = output
            .content
            .get("file_hash")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();

        let mut actions = Vec::with_capacity(occurrences.len());
        for occ in occurrences {
            let Some(n) = occ.get("occurrence").and_then(serde_json::Value::as_u64) else {
                continue;
            };
            if n == 0 {
                continue;
            }
            let line = occ
                .get("line")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let context = occ
                .get("context")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            actions.push(FollowUpAction {
                action: format!("apply_at_occurrence_{n}"),
                description: format!("Apply edit at occurrence {n} (line {line}, in {context})"),
                tool: "edit".to_string(),
                args: serde_json::json!({ "occurrence": n }),
                args_mode: crate::tool::follow_up::FollowUpArgsMode::MergeOriginal,
                expires: ExpiryCondition::FileModified {
                    path: path.clone(),
                    content_hash: file_hash.clone(),
                },
                confidence: Confidence::Medium,
                before_content: BeforeContentSource::Unavailable,
            });
        }
        actions
    }
}

/// Byte offsets of every non-overlapping match of `needle` in `haystack`,
/// left to right. Matches the counting semantics of `str::matches`. An empty
/// `needle` yields no offsets.
fn occurrence_offsets(haystack: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    haystack.match_indices(needle).map(|(i, _)| i).collect()
}

/// Builds one descriptor per match offset for the ambiguous-edit result:
/// `{ occurrence (1-based), line (1-based), context }`, where `context` is the
/// trimmed text of the line containing the match.
fn occurrence_descriptors(content: &str, offsets: &[usize]) -> Vec<serde_json::Value> {
    offsets
        .iter()
        .enumerate()
        .map(|(i, &byte)| {
            let prefix = &content[..byte];
            let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
            let line_start = prefix.rfind('\n').map_or(0, |p| p + 1);
            let line_end = content[byte..]
                .find('\n')
                .map_or(content.len(), |p| byte + p);
            let line_context = content[line_start..line_end].trim();
            serde_json::json!({
                "occurrence": i + 1,
                "line": line,
                "context": line_context,
            })
        })
        .collect()
}

/// Computes the blast radius payload for an edit:
/// `{ lines_added, lines_removed, lines_modified, containing_symbols }`.
///
/// `lines_modified` counts lines in the overlapping region of old and
/// new text — the minimum of `old_string` and `new_string` line counts.
/// `lines_added` / `lines_removed` are the net difference in total file
/// line count.
fn compute_blast_radius(
    path: &std::path::Path,
    original: &str,
    staged: &str,
    old_string: &str,
    new_string: &str,
    match_byte: usize,
) -> serde_json::Value {
    let original_lines = original.lines().count();
    let staged_lines = staged.lines().count();
    let lines_added = staged_lines.saturating_sub(original_lines);
    let lines_removed = original_lines.saturating_sub(staged_lines);

    let old_line_count = old_string.lines().count().max(1);
    let new_line_count = new_string.lines().count().max(1);
    let lines_modified = old_line_count.min(new_line_count);

    let symbols = containing_symbols(path, staged, match_byte, match_byte + new_string.len());

    serde_json::json!({
        "lines_added": lines_added,
        "lines_removed": lines_removed,
        "lines_modified": lines_modified,
        "containing_symbols": symbols,
    })
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
    use crate::tool::envelope::ToolEnvelope;

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "edit".to_string(),
            model_args: args,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn edit_args_schema_matches_previous_hand_written_schema() {
        let expected_schema = json!({
            "type": "object",
            "required": ["path", "old_string", "new_string"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file to edit."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to find and replace. Must match exactly once."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text."
                },
                "occurrence": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based occurrence to replace when old_string matches more than once. Omit for a unique match."
                }
            },
            "additionalProperties": false
        });
        assert_eq!(EditArgs::json_schema(), expected_schema);
    }

    #[tokio::test]
    async fn pre_validate_blocks_without_prior_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        tokio::fs::write(&path, "fn main() {}\n").await.unwrap();

        let tool = EditTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "fn main",
            "new_string": "fn entry"
        }));
        let ctx = ToolContext::empty();

        match tool.pre_validate(&envelope, &ctx).await {
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => {
                assert!(reason.contains("not been read"), "{reason:?}");
            }
            PreValidateOutcome::Proceed => panic!("expected Block, got Proceed"),
        }
    }

    #[tokio::test]
    async fn pre_validate_blocks_when_old_string_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        tokio::fs::write(&path, "fn main() {}\n").await.unwrap();
        let tool = EditTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "nope",
            "new_string": "anything"
        }));
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);

        match tool.pre_validate(&envelope, &ctx).await {
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => {
                assert!(reason.contains("not found"), "{reason:?}");
            }
            PreValidateOutcome::Proceed => panic!("expected Block, got Proceed"),
        }
    }

    #[tokio::test]
    async fn pre_validate_proceeds_on_ambiguous_match_without_occurrence() {
        // R2: ambiguity is no longer blocked at pre_validate — it must reach
        // execute so the apply_at_occurrence_N follow-ups can be offered.
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        tokio::fs::write(&path, "let x = 1;\nlet x = 2;\n")
            .await
            .unwrap();
        let tool = EditTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "let x",
            "new_string": "let y"
        }));
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);

        assert!(matches!(
            tool.pre_validate(&envelope, &ctx).await,
            PreValidateOutcome::Proceed
        ));
    }

    #[tokio::test]
    async fn pre_validate_blocks_occurrence_out_of_range() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        tokio::fs::write(&path, "let x = 1;\nlet x = 2;\n")
            .await
            .unwrap();
        let tool = EditTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "let x",
            "new_string": "let y",
            "occurrence": 5
        }));
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);

        match tool.pre_validate(&envelope, &ctx).await {
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => {
                assert!(reason.contains("out of range"), "{reason:?}");
            }
            PreValidateOutcome::Proceed => panic!("expected Block, got Proceed"),
        }
    }

    #[tokio::test]
    async fn execute_replaces_single_occurrence() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        tokio::fs::write(&path, "fn target() { let x = 1; }\n")
            .await
            .unwrap();

        let tool = EditTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1",
            "new_string": "let x = 2"
        }));
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(on_disk, "fn target() { let x = 2; }\n");
    }

    #[tokio::test]
    async fn post_validate_mode_is_gate() {
        let tool = EditTool::new();
        assert_eq!(tool.post_validate_mode(), PostValidateMode::Gate);
    }

    #[tokio::test]
    async fn ast_failure_without_override_preserves_original() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn main() { let x = 1; }\n";
        tokio::fs::write(&path, original).await.unwrap();

        let tool = EditTool::new();
        // Replace closing brace with semicolon to introduce a syntax error.
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1; }",
            "new_string": "let x = 1; ;"
        }));
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(out.is_error(), "expected gate failure: {:?}", out.content);
        assert_eq!(out.content["committed"], false);

        let after = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(after, original, "disk content was modified");

        let diagnostics = out.content["diagnostics"].as_array().unwrap();
        assert!(!diagnostics.is_empty(), "no diagnostics recorded");
        assert_eq!(diagnostics[0]["severity"], "error");
        assert!(diagnostics[0]["code"].as_str().is_some());
        assert!(diagnostics[0]["line"].is_u64());
    }

    #[tokio::test]
    async fn ast_failure_with_allow_broken_commits_and_records_override() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn main() { let x = 1; }\n";
        tokio::fs::write(&path, original).await.unwrap();

        let tool = EditTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1; }",
            "new_string": "let x = 1; ;"
        }));
        let mut ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        ctx.set_flag(ToolFlag::AllowBrokenAst, "test:override-broken");

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        // With AllowBrokenAst the file is committed despite AST errors;
        // diagnostics are still recorded at severity 'error' so is_error is true.
        assert!(out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["committed"], true);

        let after = tokio::fs::read_to_string(&path).await.unwrap();
        assert_ne!(after, original, "disk content unchanged despite override");

        let overrides = out.content["check_overrides"].as_array().unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0]["check_name"], "ast_validation");
        assert_eq!(overrides[0]["source"], "test:override-broken");

        let diagnostics = out.content["diagnostics"].as_array().unwrap();
        assert!(
            !diagnostics.is_empty(),
            "expected ast diagnostics still recorded"
        );
    }

    #[tokio::test]
    async fn blast_radius_reports_containing_function() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original =
            "fn target_function() {\n    let v = vec![1, 2, 3];\n    println!(\"{:?}\", v);\n}\n";
        tokio::fs::write(&path, original).await.unwrap();

        let tool = EditTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "vec![1, 2, 3]",
            "new_string": "vec![1, 2, 3, 4]"
        }));
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);

        let symbols = out.content["blast_radius"]["containing_symbols"]
            .as_array()
            .unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.as_str().unwrap_or("").contains("target_function")),
            "containing_symbols {symbols:?} missing target_function"
        );
    }

    #[tokio::test]
    async fn blast_radius_includes_diff_stats() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn a() { let x = 1; }\n";
        tokio::fs::write(&path, original).await.unwrap();

        let tool = EditTool::new();
        // Adding two newlines inside the function body — net +2 lines.
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1;",
            "new_string": "let x = 1;\n    let y = 2;\n    let z = 3;"
        }));
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        let blast = &out.content["blast_radius"];
        assert_eq!(blast["lines_added"].as_u64().unwrap(), 2);
        assert_eq!(blast["lines_removed"].as_u64().unwrap(), 0);
    }

    // --- Follow-up tests ----------------------------------------------------

    #[tokio::test]
    async fn noncommit_edit_registers_no_follow_ups() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        let original = "fn main() { let x = 1; }\n";
        tokio::fs::write(&path, original).await.unwrap();

        let tool = EditTool::new();
        // Introduce a syntax error → gate failure, committed == false.
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1; }",
            "new_string": "let x = 1; ;"
        }));
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert_eq!(out.content["committed"], false);
        assert!(tool.register_follow_ups(&out, &ctx).await.is_empty());
    }

    #[tokio::test]
    async fn ambiguous_edit_registers_apply_at_occurrence() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        tokio::fs::write(&path, "let x = 1;\nlet x = 2;\n")
            .await
            .unwrap();

        let tool = EditTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "let x",
            "new_string": "let y"
        }));
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert_eq!(out.content["committed"], false);
        assert_eq!(out.content["match_count"], 2);

        let follow_ups = tool.register_follow_ups(&out, &ctx).await;
        assert_eq!(follow_ups.len(), 2);
        assert_eq!(follow_ups[0].action, "apply_at_occurrence_1");
        assert_eq!(follow_ups[0].args, json!({ "occurrence": 1 }));
        assert_eq!(follow_ups[1].action, "apply_at_occurrence_2");
        assert_eq!(follow_ups[1].args, json!({ "occurrence": 2 }));
        for fu in &follow_ups {
            assert_eq!(fu.tool, "edit");
            assert!(matches!(fu.expires, ExpiryCondition::FileModified { .. }));
            assert!(matches!(
                fu.before_content,
                BeforeContentSource::Unavailable
            ));
        }
    }

    #[tokio::test]
    async fn single_match_registers_no_apply_at_occurrence() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.rs");
        tokio::fs::write(&path, "fn a() { let x = 1; }\n")
            .await
            .unwrap();

        let tool = EditTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1",
            "new_string": "let x = 9"
        }));
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        let follow_ups = tool.register_follow_ups(&out, &ctx).await;
        assert!(
            !follow_ups
                .iter()
                .any(|f| f.action.starts_with("apply_at_occurrence"))
        );
    }

    #[tokio::test]
    async fn occurrence_commits_selected_match() {
        let dir = tempdir().unwrap();
        // Plain-text file so AST validation is skipped and the test isolates
        // occurrence selection.
        let path = dir.path().join("notes.txt");
        tokio::fs::write(&path, "value = 1\nvalue = 1\n")
            .await
            .unwrap();

        let tool = EditTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "value = 1",
            "new_string": "value = 9",
            "occurrence": 2
        }));
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert_eq!(out.content["committed"], true);
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(on_disk, "value = 1\nvalue = 9\n");
    }

    // --- Workspace confinement -------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn confined_context_refuses_symlink_escape() {
        let outer = tempdir().unwrap();
        let root = outer.path().join("ws");
        let elsewhere = outer.path().join("elsewhere");
        tokio::fs::create_dir(&root).await.unwrap();
        tokio::fs::create_dir(&elsewhere).await.unwrap();
        let target = elsewhere.join("target.txt");
        tokio::fs::write(&target, "secret = 1\n").await.unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();

        let tool = EditTool::new();
        let mut ctx = ToolContext::empty();
        ctx.confine_to_workspace(root.clone());
        ctx.set_working_dir(root.clone());
        // Even a previously-read file must be refused when reached through
        // an escaping symlink.
        ctx.mark_file_read(&root.join("link/target.txt"));
        let envelope = envelope_for(json!({
            "path": root.join("link/target.txt").to_string_lossy(),
            "old_string": "secret = 1",
            "new_string": "secret = 2",
        }));

        match tool.pre_validate(&envelope, &ctx).await {
            PreValidateOutcome::Block(crate::tool::lifecycle::BlockDecision {
                message: reason,
                ..
            }) => {
                assert!(reason.contains("outside the workspace"), "{reason}");
            }
            PreValidateOutcome::Proceed => panic!("expected Block"),
        }
        let err = tool.execute(&envelope, &ctx).await.expect_err("refused");
        assert!(err.to_string().contains("outside the workspace"), "{err}");
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "secret = 1\n",
            "target outside the root untouched"
        );
    }

    #[tokio::test]
    async fn confined_context_allows_edit_inside_root() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("inside.txt");
        tokio::fs::write(&path, "value = 1\n").await.unwrap();

        let tool = EditTool::new();
        let mut ctx = ToolContext::empty();
        ctx.confine_to_workspace(dir.path().to_path_buf());
        ctx.set_working_dir(dir.path().to_path_buf());
        ctx.mark_file_read(&path);
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "old_string": "value = 1",
            "new_string": "value = 2",
        }));

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert_eq!(out.content["committed"], true);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "value = 2\n"
        );
    }
}
