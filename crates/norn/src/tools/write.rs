//! Write tool — read-before-overwrite gate, AST validation (report
//! semantics), and configurable file-length enforcement.
//!
//! `Write` always commits to disk (report semantics): the AST and
//! length checks describe what was written, they do not gate the
//! write itself. The orchestrator's `AllowOverwrite` flag suppresses
//! the read-before-overwrite check and records a `CheckOverride` in
//! the tool output so reviewers can audit overrides.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;

use super::ast::{AstCheck, SyntaxError, check_syntax};
use super::confinement::check_confinement;
use super::file_commit::commit_file_atomic;
use super::validation::count_code_lines;
use crate::error::ToolError;
use crate::tool::context::{ToolContext, ToolFlag};
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::lifecycle::{
    BlockDecision, CheckOverride, PostValidateMode, PostValidateOutcome, PreValidateOutcome,
};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Configurable file-length limit for `Write`.
///
/// `default` applies to any path that does not match an `overrides`
/// pattern. Override patterns are evaluated in order; the first
/// matching pattern wins. When `default` is `None` and no override
/// matches, the length check is skipped entirely — there is no
/// hardcoded fallback.
pub struct LengthLimit {
    /// Code-line cap applied when no `overrides` entry matches. `None`
    /// disables the check for non-matching paths.
    pub default: Option<usize>,
    /// Glob patterns paired with their own code-line caps.
    pub overrides: Vec<(glob::Pattern, usize)>,
}

impl LengthLimit {
    /// Returns a length limit with no default and no overrides — the
    /// check is fully disabled until the caller installs a value.
    #[must_use]
    pub fn none() -> Self {
        Self {
            default: None,
            overrides: Vec::new(),
        }
    }

    /// Adds an override entry.
    #[must_use]
    pub fn with_override(mut self, pattern: glob::Pattern, limit: usize) -> Self {
        self.overrides.push((pattern, limit));
        self
    }

    /// Returns the limit applicable to `path`, or `None` when no check
    /// should run.
    #[must_use]
    pub fn limit_for(&self, path: &Path) -> Option<usize> {
        let path_str = path.to_string_lossy();
        for (pattern, limit) in &self.overrides {
            if pattern.matches(&path_str) {
                return Some(*limit);
            }
        }
        self.default
    }
}

impl Default for LengthLimit {
    fn default() -> Self {
        Self::none()
    }
}

/// Writes a file to disk after enforcing read-before-overwrite and
/// reporting AST and length issues in the tool result.
pub struct WriteTool {
    length_limit: LengthLimit,
}

impl WriteTool {
    /// Constructs a `WriteTool` with no length-limit configured. Callers
    /// that want enforcement must supply a [`LengthLimit`] via
    /// [`Self::with_length_limit`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            length_limit: LengthLimit::none(),
        }
    }

    /// Constructs a `WriteTool` with an explicit length limit
    /// configuration.
    #[must_use]
    pub fn with_length_limit(length_limit: LengthLimit) -> Self {
        Self { length_limit }
    }
}

impl Default for WriteTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }

    fn description(&self) -> &'static str {
        include_str!("guidance/write.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::FileSystem
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("guidance/write.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file to write."
                },
                "content": {
                    "type": "string",
                    "description": "Complete file content to write."
                }
            },
            "additionalProperties": false
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Write
    }

    fn post_validate_mode(&self) -> PostValidateMode {
        PostValidateMode::Report
    }

    async fn pre_validate(&self, envelope: &ToolEnvelope, ctx: &ToolContext) -> PreValidateOutcome {
        let args: WriteArgs = match serde_json::from_value(envelope.model_args.clone()) {
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
                BlockDecision::new(format!("Write blocked: {reason}"))
                    .with_kind(ToolErrorKind::PermissionDenied)
                    .with_detail(serde_json::json!({ "path": args.path })),
            );
        }

        if ctx.has_flag(&ToolFlag::AllowOverwrite) {
            // Override path — execute will record the CheckOverride.
            return PreValidateOutcome::Proceed;
        }

        let exists = tokio::fs::try_exists(&path).await.unwrap_or(false);
        if !exists {
            return PreValidateOutcome::Proceed;
        }

        if ctx.has_read_file(&path) {
            return PreValidateOutcome::Proceed;
        }

        PreValidateOutcome::Block(
            BlockDecision::new(format!(
                "Write blocked: file {} exists and has not been read this session",
                args.path
            ))
            .with_guidance(
                "Read the file with the read tool first, or have the orchestrator set \
                 the AllowOverwrite flag.",
            )
            .with_detail(serde_json::json!({ "path": args.path })),
        )
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: WriteArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("invalid arguments: {e}"),
            }
        })?;
        let path = ctx.resolve_path(&args.path);
        let _descriptor_permit = crate::resource::acquire_private_fs()
            .map_err(|error| ToolError::DescriptorAdmission(Box::new(error)))?;

        // Workspace confinement (opt-in): re-checked at execute time so a
        // direct invocation cannot bypass the pre_validate gate.
        if let Err(reason) = check_confinement(ctx, &path) {
            return Err(ToolError::ExecutionFailed {
                reason: format!("write refused: {reason}"),
            });
        }

        // Record any compile-time check that was overridden by an
        // orchestrator flag. This must happen before the write so the
        // override is captured even if the write fails.
        let mut check_overrides: Vec<CheckOverride> = Vec::new();
        if ctx.has_flag(&ToolFlag::AllowOverwrite) {
            let exists = tokio::fs::try_exists(&path).await.unwrap_or(false);
            if exists && !ctx.has_read_file(&path) {
                check_overrides.push(CheckOverride {
                    check_name: "read_before_overwrite".to_string(),
                    flag: ToolFlag::AllowOverwrite,
                    source: ctx
                        .flag_source(&ToolFlag::AllowOverwrite)
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Err(ToolError::ExecutionFailed {
                reason: format!("failed to create parent directories for {}: {e}", args.path),
            });
        }

        // Atomic commit: temp file in the same directory + rename, keeping
        // an existing target's permission bits. ENOSPC or a crash
        // mid-commit leaves the original content untouched.
        if let Err(e) = commit_file_atomic(&path, args.content.as_bytes()).await {
            return Err(ToolError::ExecutionFailed {
                reason: format!("failed to write {}: {e}", args.path),
            });
        }

        // Report semantics: file is on disk. AST and length checks
        // describe what was written, they do not gate the write.
        let mut diagnostics: Vec<serde_json::Value> = Vec::new();

        match check_syntax(&path, &args.content) {
            AstCheck::Pass | AstCheck::Unsupported => {}
            AstCheck::Fail { errors: ast_errors } => {
                for err in &ast_errors {
                    diagnostics.push(syntax_error_to_diagnostic(err));
                }
            }
        }

        let line_count = count_code_lines(&path, &args.content);
        let limit = self.length_limit.limit_for(&path);
        let length_limit_value = match limit {
            Some(limit_value) => {
                let limit_u64 = u64::try_from(limit_value).unwrap_or(u64::MAX);
                if line_count > limit_u64 {
                    diagnostics.push(serde_json::json!({
                        "code": "file-length-exceeded",
                        "line": 1u64,
                        "severity": "warning",
                        "message": format!(
                            "file exceeds limit: {line_count} lines (limit {limit_value})"
                        ),
                    }));
                }
                serde_json::Value::from(limit_value)
            }
            None => serde_json::Value::Null,
        };

        let has_error_diagnostic = diagnostics
            .iter()
            .any(|d| d.get("severity").and_then(serde_json::Value::as_str) == Some("error"));

        let payload = serde_json::json!({
            "path": args.path,
            "bytes_written": args.content.len(),
            "line_count": line_count,
            "length_limit": length_limit_value,
            "diagnostics": diagnostics,
            "check_overrides": check_overrides,
        });

        if has_error_diagnostic {
            return Ok(ToolOutput::failure_with_content(
                payload,
                ToolErrorPayload::new(
                    ToolErrorKind::ValidationFailed,
                    "file written with syntax errors",
                )
                .with_detail(serde_json::json!({ "diagnostics": diagnostics })),
            ));
        }
        Ok(ToolOutput::success(payload))
    }

    async fn post_validate(&self, output: &ToolOutput, _ctx: &ToolContext) -> PostValidateOutcome {
        // Re-surface the structured errors recorded during execute so
        // runtime introspection can act on them without re-parsing the file.
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
}

/// Builds a structured diagnostic JSON object from a [`SyntaxError`].
pub(super) fn syntax_error_to_diagnostic(err: &SyntaxError) -> serde_json::Value {
    let code = if err.missing {
        "syntax-missing"
    } else {
        "syntax-error"
    };
    let line_u64 = u64::try_from(err.line).unwrap_or(u64::MAX);
    serde_json::json!({
        "code": code,
        "line": line_u64,
        "severity": "error",
        "message": err.render(),
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
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use std::fmt::Write as _;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::tool::envelope::ToolEnvelope;

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "write".to_string(),
            model_args: args,
            metadata: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn pre_validate_new_file_proceeds_without_prior_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("new.txt");

        let tool = WriteTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "content": "hello\n"
        }));
        let ctx = ToolContext::empty();

        match tool.pre_validate(&envelope, &ctx).await {
            PreValidateOutcome::Proceed => {}
            PreValidateOutcome::Block(decision) => {
                panic!("expected Proceed, got Block({:?})", decision.message)
            }
        }
    }

    #[tokio::test]
    async fn pre_validate_existing_unread_file_blocks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("existing.txt");
        tokio::fs::write(&path, "old\n").await.unwrap();

        let tool = WriteTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "content": "new\n"
        }));
        let ctx = ToolContext::empty();

        match tool.pre_validate(&envelope, &ctx).await {
            PreValidateOutcome::Block(decision) => {
                assert!(
                    decision.message.contains("not been read"),
                    "message was {:?}",
                    decision.message
                );
            }
            PreValidateOutcome::Proceed => panic!("expected Block, got Proceed"),
        }

        ctx.mark_file_read(&path);
        match tool.pre_validate(&envelope, &ctx).await {
            PreValidateOutcome::Proceed => {}
            PreValidateOutcome::Block(decision) => {
                panic!(
                    "expected Proceed after mark_file_read, got Block({:?})",
                    decision.message
                )
            }
        }
    }

    #[tokio::test]
    async fn allow_overwrite_flag_skips_read_check_and_records_override() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("existing.txt");
        tokio::fs::write(&path, "old\n").await.unwrap();

        let tool = WriteTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "content": "fn main() {}\n"
        }));
        let mut ctx = ToolContext::empty();
        ctx.set_flag(ToolFlag::AllowOverwrite, "test:override-source");

        match tool.pre_validate(&envelope, &ctx).await {
            PreValidateOutcome::Proceed => {}
            PreValidateOutcome::Block(decision) => {
                panic!(
                    "expected Proceed under AllowOverwrite, got Block({:?})",
                    decision.message
                )
            }
        }

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(!out.is_error(), "expected success: {:?}", out.content);
        let overrides = out.content["check_overrides"].as_array().unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0]["check_name"], "read_before_overwrite");
        assert_eq!(overrides[0]["source"], "test:override-source");
    }

    #[tokio::test]
    async fn post_validate_mode_is_report() {
        let tool = WriteTool::new();
        assert_eq!(tool.post_validate_mode(), PostValidateMode::Report);
    }

    #[tokio::test]
    async fn invalid_rust_reports_failure_with_line_number() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.rs");

        let tool = WriteTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "content": "fn main() {\n    let x = 1;\n"
        }));
        let ctx = ToolContext::empty();

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(out.is_error());
        let diagnostics = out.content["diagnostics"].as_array().unwrap();
        assert!(!diagnostics.is_empty());
        let first = &diagnostics[0];
        assert_eq!(first["severity"], "error");
        assert!(
            first["code"]
                .as_str()
                .is_some_and(|c| c.starts_with("syntax-"))
        );
        assert!(first["line"].is_u64());
        let message = first["message"].as_str().unwrap();
        assert!(message.contains("line "), "no line number in {message}");

        // File was still written (report semantics).
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(on_disk, "fn main() {\n    let x = 1;\n");

        match tool.post_validate(&out, &ctx).await {
            PostValidateOutcome::Fail { errors } => {
                assert!(!errors.is_empty());
            }
            PostValidateOutcome::Pass => panic!("expected Fail, got Pass"),
        }
    }

    #[tokio::test]
    async fn valid_rust_passes_post_validate() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("good.rs");

        let tool = WriteTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "content": "fn main() { let _x = 1; }\n"
        }));
        let ctx = ToolContext::empty();

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        let diagnostics = out.content["diagnostics"].as_array().unwrap();
        assert!(diagnostics.is_empty());

        match tool.post_validate(&out, &ctx).await {
            PostValidateOutcome::Pass => {}
            PostValidateOutcome::Fail { errors } => panic!("expected Pass, got Fail({errors:?})"),
        }
    }

    #[tokio::test]
    async fn unsupported_extension_passes_post_validate() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("notes.txt");

        let tool = WriteTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "content": "free-form text\n"
        }));
        let ctx = ToolContext::empty();

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
    }

    #[tokio::test]
    async fn no_length_limit_skips_check_even_on_long_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("big.unk");
        let mut body = String::new();
        for i in 1..=1_000 {
            let _ = writeln!(body, "line{i}");
        }

        let tool = WriteTool::new();
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "content": body
        }));
        let ctx = ToolContext::empty();

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(
            !out.is_error(),
            "no limit means no error: {:?}",
            out.content
        );
        assert!(out.content["length_limit"].is_null());
        let diagnostics = out.content["diagnostics"].as_array().unwrap();
        assert!(diagnostics.iter().all(|d| {
            d.get("code").and_then(serde_json::Value::as_str) != Some("file-length-exceeded")
        }));
    }

    #[tokio::test]
    async fn caller_configured_limit_reports_violation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("big.unk");
        let mut body = String::new();
        for i in 1..=600 {
            let _ = writeln!(body, "line{i}");
        }

        let tool = WriteTool::with_length_limit(LengthLimit {
            default: Some(500),
            overrides: Vec::new(),
        });
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "content": body
        }));
        let ctx = ToolContext::empty();

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        // Length violation is severity=warning so is_error is false.
        let diagnostics = out.content["diagnostics"].as_array().unwrap();
        let length_diag = diagnostics
            .iter()
            .find(|d| {
                d.get("code").and_then(serde_json::Value::as_str) == Some("file-length-exceeded")
            })
            .expect("expected file-length-exceeded diagnostic");
        assert_eq!(length_diag["severity"], "warning");
        assert_eq!(length_diag["line"], 1);
        let msg = length_diag["message"].as_str().unwrap();
        assert!(msg.contains("600") && msg.contains("500"));
    }

    #[tokio::test]
    async fn caller_configured_limit_passes_when_within_bounds() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ok.unk");
        let mut body = String::new();
        for i in 1..=400 {
            let _ = writeln!(body, "line{i}");
        }

        let tool = WriteTool::with_length_limit(LengthLimit {
            default: Some(500),
            overrides: Vec::new(),
        });
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "content": body
        }));
        let ctx = ToolContext::empty();

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        let diagnostics = out.content["diagnostics"].as_array().unwrap();
        assert!(diagnostics.iter().all(|d| {
            d.get("code").and_then(serde_json::Value::as_str) != Some("file-length-exceeded")
        }));
    }

    #[test]
    fn length_limit_none_returns_none_for_any_path() {
        let limit = LengthLimit::none();
        assert!(
            limit
                .limit_for(Path::new("crates/foo/src/lib.rs"))
                .is_none()
        );
        assert!(limit.limit_for(Path::new("/anywhere/else")).is_none());
    }

    #[test]
    fn length_limit_with_caller_default_returns_that_default() {
        let limit = LengthLimit {
            default: Some(500),
            overrides: Vec::new(),
        };
        assert_eq!(
            limit.limit_for(Path::new("crates/foo/src/lib.rs")),
            Some(500)
        );
    }

    #[test]
    fn length_limit_override_matches_even_with_no_default() {
        let limit = LengthLimit {
            default: None,
            overrides: vec![(glob::Pattern::new("**/tests/**").unwrap(), 2_000)],
        };
        assert_eq!(
            limit.limit_for(Path::new("crates/foo/tests/integration.rs")),
            Some(2_000)
        );
        assert!(
            limit
                .limit_for(Path::new("crates/foo/src/lib.rs"))
                .is_none()
        );
    }

    #[test]
    fn length_limit_overrides_match_first() {
        let limit = LengthLimit {
            default: Some(500),
            overrides: Vec::new(),
        }
        .with_override(glob::Pattern::new("**/tests/**").unwrap(), 5_000)
        .with_override(glob::Pattern::new("**/*_test.rs").unwrap(), 2_000);

        assert_eq!(
            limit.limit_for(Path::new("crates/foo/tests/integration.rs")),
            Some(5_000)
        );
        assert_eq!(
            limit.limit_for(Path::new("crates/foo/src/widget_test.rs")),
            Some(2_000)
        );
        assert_eq!(
            limit.limit_for(Path::new("crates/foo/src/lib.rs")),
            Some(500)
        );
    }

    // --- Workspace confinement -------------------------------------------

    #[tokio::test]
    async fn confined_context_refuses_dot_dot_escape() {
        let outer = tempdir().unwrap();
        let root = outer.path().join("ws");
        tokio::fs::create_dir(&root).await.unwrap();
        let escape_target = outer.path().join("escape.txt");

        let tool = WriteTool::new();
        let mut ctx = ToolContext::empty();
        ctx.confine_to_workspace(root.clone());
        ctx.set_working_dir(root.clone());
        let envelope = envelope_for(json!({
            "path": "../escape.txt",
            "content": "should never land"
        }));

        match tool.pre_validate(&envelope, &ctx).await {
            PreValidateOutcome::Block(decision) => {
                assert!(
                    decision.message.contains("outside the workspace"),
                    "{}",
                    decision.message
                );
            }
            PreValidateOutcome::Proceed => panic!("expected Block"),
        }
        let err = tool.execute(&envelope, &ctx).await.expect_err("refused");
        assert!(err.to_string().contains("outside the workspace"), "{err}");
        assert!(!escape_target.exists(), "escape file must not be created");
    }

    #[tokio::test]
    async fn confined_context_refuses_absolute_escape() {
        let outer = tempdir().unwrap();
        let root = outer.path().join("ws");
        tokio::fs::create_dir(&root).await.unwrap();
        let escape_target = outer.path().join("abs.txt");

        let tool = WriteTool::new();
        let mut ctx = ToolContext::empty();
        ctx.confine_to_workspace(root.clone());
        ctx.set_working_dir(root.clone());
        let envelope = envelope_for(json!({
            "path": escape_target.to_string_lossy(),
            "content": "nope"
        }));

        match tool.pre_validate(&envelope, &ctx).await {
            PreValidateOutcome::Block(decision) => {
                assert!(
                    decision.message.contains("outside the workspace"),
                    "{}",
                    decision.message
                );
            }
            PreValidateOutcome::Proceed => panic!("expected Block"),
        }
        assert!(!escape_target.exists());
    }

    /// DECISIONS §0.6(b): the read carve-out is read-only. Even with a
    /// read-exempt root declared, WRITE into that exempt dir is refused —
    /// write never consults the exemption.
    #[tokio::test]
    async fn confined_write_refuses_read_exempt_dir() {
        let outer = tempdir().unwrap();
        let root = outer.path().join("ws");
        let skills = outer.path().join("home-skills");
        tokio::fs::create_dir(&root).await.unwrap();
        tokio::fs::create_dir(&skills).await.unwrap();
        let target = skills.join("SKILL.md");

        let tool = WriteTool::new();
        let mut ctx = ToolContext::empty();
        ctx.confine_to_workspace(root.clone());
        ctx.set_working_dir(root.clone());
        ctx.set_read_exempt_roots(vec![skills.clone()]);

        let envelope = envelope_for(json!({
            "path": target.to_string_lossy(),
            "content": "must never land in an exempt dir"
        }));
        match tool.pre_validate(&envelope, &ctx).await {
            PreValidateOutcome::Block(decision) => {
                assert!(
                    decision.message.contains("outside the workspace"),
                    "{}",
                    decision.message
                );
            }
            PreValidateOutcome::Proceed => panic!("expected Block for write into exempt dir"),
        }
        let err = tool.execute(&envelope, &ctx).await.expect_err("refused");
        assert!(err.to_string().contains("outside the workspace"), "{err}");
        assert!(
            !target.exists(),
            "write into an exempt dir must not create the file"
        );
    }

    #[tokio::test]
    async fn confined_context_allows_write_inside_root() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("inside.txt");
        let tool = WriteTool::new();
        let mut ctx = ToolContext::empty();
        ctx.confine_to_workspace(dir.path().to_path_buf());
        ctx.set_working_dir(dir.path().to_path_buf());
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "content": "fine"
        }));

        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "fine");
    }

    // --- Atomic commit ------------------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn overwrite_preserves_permission_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("script.sh");
        tokio::fs::write(&path, "#!/bin/sh\n").await.unwrap();
        let mut perms = tokio::fs::metadata(&path).await.unwrap().permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&path, perms).await.unwrap();

        let tool = WriteTool::new();
        let ctx = ToolContext::empty();
        ctx.mark_file_read(&path);
        let envelope = envelope_for(json!({
            "path": path.to_string_lossy(),
            "content": "#!/bin/sh\necho updated\n"
        }));
        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);

        let mode = tokio::fs::metadata(&path).await.unwrap().permissions();
        assert_eq!(
            mode.mode() & 0o7777,
            0o755,
            "atomic rename must keep the executable bit"
        );
    }
}
