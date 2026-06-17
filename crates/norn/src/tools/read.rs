//! Read tool.
//!
//! Reads a UTF-8 text file from disk, returning its contents in `cat -n`
//! style (one-based line numbers, tab-separated). Detects binary files
//! and image extensions and reports them with a descriptive payload
//! instead of raw bytes. Successful reads are recorded in `ToolContext`
//! so the Write and Edit tools can enforce read-before-modify.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;

use super::confinement::check_confinement;
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::output_budget::ToolOutputBudget;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Image file extensions reported with the `image` kind instead of being read.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "svg", "webp"];

/// Number of leading bytes scanned for null bytes when classifying binary files.
const BINARY_SCAN_BYTES: usize = 8192;

/// Reads a file from disk and returns its content with line numbers.
///
/// `effect` is `ReadOnly`: Read calls are scheduled concurrently with
/// other read-only tools. `on_success` registers the path in the
/// `ToolContext` so Write and Edit can enforce read-before-modify.
pub struct ReadTool;

impl ReadTool {
    /// Constructs a stateless Read tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        include_str!("guidance/read.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::FileSystem
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("guidance/read.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative file path." },
                "offset": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based starting line. Defaults to 1."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of lines to return. Defaults to a bounded first window; large requests are capped."
                }
            }
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReadArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("invalid arguments: {e}"),
            }
        })?;

        let path = ctx.resolve_path(&args.path);

        // Workspace confinement (opt-in): refuse before touching disk so
        // even metadata of out-of-root paths is never disclosed.
        if let Err(reason) = check_confinement(ctx, &path) {
            return Ok(ToolOutput::failure_with_content(
                serde_json::json!({ "path": args.path, "kind": "confinement_refused" }),
                ToolErrorPayload::new(
                    ToolErrorKind::PermissionDenied,
                    format!("read refused: {reason}"),
                )
                .with_detail(serde_json::json!({ "path": args.path })),
            ));
        }

        // Image extension takes precedence over reading bytes.
        if is_image(&path) {
            let payload = serde_json::json!({
                "path": args.path,
                "kind": "image",
                "message": format!("image file: {}", args.path),
            });
            return Ok(ToolOutput::success(payload));
        }

        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(e) => {
                return Ok(ToolOutput::failure_with_content(
                    serde_json::json!({ "path": args.path, "kind": "io_error" }),
                    ToolErrorPayload::new(ToolErrorKind::Io, e.to_string())
                        .with_detail(serde_json::json!({ "path": args.path })),
                ));
            }
        };

        if is_binary(&bytes) {
            let payload = serde_json::json!({
                "path": args.path,
                "kind": "binary",
                "size_bytes": bytes.len(),
                "message": format!("binary file ({} bytes)", bytes.len()),
            });
            return Ok(ToolOutput::success(payload));
        }

        let text = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(e) => {
                let payload = serde_json::json!({
                    "path": args.path,
                    "kind": "binary",
                    "message": format!("file is not valid UTF-8: {e}"),
                });
                return Ok(ToolOutput::success(payload));
            }
        };

        let budget = read_budget(ctx);
        let rendered = render_with_line_numbers(text, args.offset, args.limit, budget);
        let warnings = read_warnings(&path, text, &rendered, budget);

        let payload = serde_json::json!({
            "path": args.path,
            "kind": "text",
            "content": rendered.content,
            "offset": rendered.offset,
            "requested_limit": args.limit,
            "effective_line_limit": rendered.effective_line_limit,
            "content_char_limit": rendered.content_char_limit,
            "returned_lines": rendered.returned_lines,
            "total_lines": rendered.total_lines,
            "file_size_bytes": bytes.len(),
            "content_chars": rendered.content_chars,
            "max_line_chars": rendered.max_line_chars,
            "next_offset": rendered.next_offset,
            "truncated": rendered.truncated(),
            "truncated_by": rendered.truncated_by(),
            "truncated_long_lines": rendered.truncated_long_lines,
            "warnings": warnings,
        });

        Ok(ToolOutput::success(payload))
    }

    async fn on_success(&self, output: &ToolOutput, ctx: &ToolContext) {
        if output.is_error() {
            return;
        }
        let Some(path_str) = output
            .content
            .get("path")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        // Only register on read kinds where the path is a real file we
        // successfully consumed; image and binary classifications still
        // count — the agent has now seen the metadata for that path.
        let kind = output
            .content
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if matches!(kind, "text" | "binary" | "image") {
            ctx.mark_file_read(Path::new(path_str));
        }
    }
}

/// Returns true when `path` ends in an image extension (case-insensitive).
fn is_image(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    let lower = ext.to_ascii_lowercase();
    IMAGE_EXTENSIONS.iter().any(|e| *e == lower)
}

/// Heuristic: a file is treated as binary if any of the first
/// `BINARY_SCAN_BYTES` bytes is `0u8`.
fn is_binary(bytes: &[u8]) -> bool {
    let scan = &bytes[..bytes.len().min(BINARY_SCAN_BYTES)];
    scan.contains(&0u8)
}

#[derive(Debug)]
struct RenderedRead {
    content: String,
    offset: u64,
    effective_line_limit: u64,
    content_char_limit: usize,
    returned_lines: u64,
    total_lines: u64,
    content_chars: usize,
    max_line_chars: usize,
    next_offset: Option<u64>,
    truncated_by_line_limit: bool,
    truncated_by_char_limit: bool,
    truncated_long_lines: u64,
}

impl RenderedRead {
    fn truncated(&self) -> bool {
        self.truncated_by_line_limit
            || self.truncated_by_char_limit
            || self.truncated_long_lines > 0
    }

    fn truncated_by(&self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if self.truncated_by_line_limit {
            reasons.push("line_limit");
        }
        if self.truncated_by_char_limit {
            reasons.push("char_limit");
        }
        if self.truncated_long_lines > 0 {
            reasons.push("long_line_limit");
        }
        reasons
    }
}

fn read_budget(ctx: &ToolContext) -> ToolOutputBudget {
    ctx.get_extension::<ToolOutputBudget>()
        .map_or_else(ToolOutputBudget::default, |budget| *budget)
}

/// Renders `text` in `cat -n` format. `offset` is the 1-based starting line
/// (defaults to 1). `limit` caps lines but cannot bypass the character budget.
fn render_with_line_numbers(
    text: &str,
    offset: Option<u64>,
    limit: Option<u64>,
    budget: ToolOutputBudget,
) -> RenderedRead {
    let offset_usize = match offset {
        None | Some(0) => 1usize,
        Some(n) => usize::try_from(n).unwrap_or(usize::MAX),
    };
    let offset_u64 = u64::try_from(offset_usize).unwrap_or(u64::MAX);
    let effective_line_limit = limit
        .unwrap_or(budget.read_default_line_limit)
        .min(budget.read_hard_line_limit);
    let content_char_limit = budget
        .read_output_char_limit
        .min(budget.read_hard_output_char_limit);
    let total_lines = u64::try_from(text.split_inclusive('\n').count()).unwrap_or(u64::MAX);

    let mut out = String::new();
    let skip = offset_usize.saturating_sub(1);
    let mut content_chars = 0usize;
    let mut returned_lines = 0u64;
    let mut next_offset = None;
    let mut truncated_by_line_limit = false;
    let mut truncated_by_char_limit = false;
    let mut truncated_long_lines = 0u64;
    let mut max_line_chars = 0usize;

    for (idx, line) in text.split_inclusive('\n').enumerate().skip(skip) {
        if returned_lines >= effective_line_limit {
            truncated_by_line_limit = true;
            next_offset = Some(u64::try_from(idx + 1).unwrap_or(u64::MAX));
            break;
        }
        let lineno = idx + 1;
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        let original_line_chars = trimmed.chars().count();
        max_line_chars = max_line_chars.max(original_line_chars);
        let (line_text, line_truncated) = cap_line(trimmed, budget.read_line_char_limit);
        if line_truncated {
            truncated_long_lines = truncated_long_lines.saturating_add(1);
        }
        let suffix = if line_truncated {
            format!(" … [line truncated; original_chars={original_line_chars}]")
        } else {
            String::new()
        };
        let candidate = format!("{lineno}\t{line_text}{suffix}\n");
        let candidate_chars = candidate.chars().count();
        if content_chars.saturating_add(candidate_chars) > content_char_limit {
            truncated_by_char_limit = true;
            next_offset = Some(u64::try_from(idx + 1).unwrap_or(u64::MAX));
            break;
        }

        out.push_str(&candidate);
        content_chars = content_chars.saturating_add(candidate_chars);
        returned_lines = returned_lines.saturating_add(1);
    }

    RenderedRead {
        content: out,
        offset: offset_u64,
        effective_line_limit,
        content_char_limit,
        returned_lines,
        total_lines,
        content_chars,
        max_line_chars,
        next_offset,
        truncated_by_line_limit,
        truncated_by_char_limit,
        truncated_long_lines,
    }
}

fn cap_line(line: &str, limit: usize) -> (String, bool) {
    let original_chars = line.chars().count();
    if original_chars <= limit {
        return (line.to_owned(), false);
    }
    (line.chars().take(limit).collect(), true)
}

fn read_warnings(
    path: &Path,
    text: &str,
    rendered: &RenderedRead,
    budget: ToolOutputBudget,
) -> Vec<serde_json::Value> {
    let mut warnings = Vec::new();
    if rendered.max_line_chars > budget.read_line_char_limit {
        warnings.push(serde_json::json!({
            "kind": "long_line",
            "message": "At least one physical line exceeds the per-line read budget; long lines are sampled rather than returned in full.",
            "max_line_chars": rendered.max_line_chars,
            "line_char_limit": budget.read_line_char_limit,
        }));
    }
    warnings.extend(build_artifact_warnings(path, text));
    warnings
}

fn build_artifact_warnings(path: &Path, text: &str) -> Vec<serde_json::Value> {
    let mut warnings = Vec::new();
    let path_text = normalized_path_text(path);
    if path_text.contains("/target/") {
        warnings.push(noise_warning(
            "rust_build_artifact_path",
            "The file path is inside target/, which is usually generated Rust build output.",
        ));
    }
    if path_text.contains("/node_modules/") {
        warnings.push(noise_warning(
            "dependency_tree_path",
            "The file path is inside node_modules/, which is usually dependency output.",
        ));
    }
    let fingerprint_hits = text.matches("target/debug/.fingerprint").count()
        + text.matches("target/release/.fingerprint").count()
        + text.matches(".fingerprint/").count();
    if fingerprint_hits >= 5 {
        warnings.push(serde_json::json!({
            "kind": "rust_fingerprint_noise",
            "message": "Content is dominated by Rust Cargo fingerprint/build-artifact paths; prefer a narrower grep/search or exclude target/.",
            "matches": fingerprint_hits,
        }));
    }
    warnings
}

fn normalized_path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn noise_warning(kind: &str, message: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": kind,
        "message": message,
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
    use std::fmt::Write as _;
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            model_args: args,
            runtime_inputs: RuntimeInputs::default(),
            metadata: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn reads_text_with_line_numbers() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        tokio::fs::write(&path, "alpha\nbeta\ngamma\ndelta\nepsilon\n")
            .await
            .unwrap();

        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        assert!(!out.is_error());
        assert_eq!(out.content["kind"], "text");
        let content = out.content["content"].as_str().unwrap();
        assert!(content.starts_with("1\talpha\n"));
        assert!(content.contains("2\tbeta\n"));
        assert!(content.contains("5\tepsilon\n"));
        assert!(out.duration < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn binary_file_reports_binary_kind() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        let data: Vec<u8> = vec![0x00, 0x01, 0x02, 0x00, b'h', b'i'];
        tokio::fs::write(&path, &data).await.unwrap();

        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        assert!(!out.is_error());
        assert_eq!(out.content["kind"], "binary");
    }

    #[tokio::test]
    async fn image_extension_returns_image_kind_case_insensitive() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("foo.PNG");
        // Write something that would otherwise be treated as text — the
        // image extension takes precedence and bytes are not read.
        tokio::fs::write(&path, b"unused").await.unwrap();

        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        assert!(!out.is_error());
        assert_eq!(out.content["kind"], "image");
    }

    #[tokio::test]
    async fn offset_and_limit_select_a_window() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("five.txt");
        tokio::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n")
            .await
            .unwrap();

        let tool = ReadTool::new();
        let envelope =
            envelope_for(json!({ "path": path.to_string_lossy(), "offset": 2, "limit": 2 }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        assert!(!out.is_error());
        let content = out.content["content"].as_str().unwrap();
        assert!(content.contains("2\ttwo\n"));
        assert!(content.contains("3\tthree\n"));
        assert!(!content.contains("1\tone"));
        assert!(!content.contains("4\tfour"));
    }

    #[tokio::test]
    async fn offset_ten_limit_five_returns_lines_ten_to_fourteen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("twenty.txt");
        let mut body = String::new();
        for i in 1..=20 {
            let _ = writeln!(body, "line{i}");
        }
        tokio::fs::write(&path, &body).await.unwrap();

        let tool = ReadTool::new();
        let envelope =
            envelope_for(json!({ "path": path.to_string_lossy(), "offset": 10, "limit": 5 }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        let content = out.content["content"].as_str().unwrap();
        for n in 10..=14 {
            assert!(
                content.contains(&format!("{n}\tline{n}\n")),
                "missing line{n} in {content:?}"
            );
        }
        assert!(!content.contains("9\tline9"));
        assert!(!content.contains("15\tline15"));
    }

    #[tokio::test]
    async fn no_limit_defaults_to_bounded_first_window() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("many.txt");
        let mut body = String::new();
        for i in 1..=300 {
            let _ = writeln!(body, "line{i}");
        }
        tokio::fs::write(&path, &body).await.unwrap();

        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        let content = out.content["content"].as_str().unwrap();
        assert!(content.contains("200\tline200\n"));
        assert!(!content.contains("201\tline201\n"));
        assert_eq!(out.content["truncated"], true);
        assert_eq!(out.content["next_offset"], 201);
        assert_eq!(out.content["returned_lines"], 200);
        assert_eq!(out.content["total_lines"], 300);
    }

    #[tokio::test]
    async fn read_caps_long_physical_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("long-line.txt");
        tokio::fs::write(&path, format!("{}\nshort\n", "x".repeat(200)))
            .await
            .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(ToolOutputBudget {
            read_default_line_limit: 200,
            read_hard_line_limit: 250,
            read_output_char_limit: 8_000,
            read_hard_output_char_limit: 8_000,
            read_line_char_limit: 40,
            model_output_inline_char_limit: 64_000,
        }));
        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        let content = out.content["content"].as_str().unwrap();
        assert!(content.contains("[line truncated; original_chars=200]"));
        assert_eq!(out.content["truncated"], true);
        assert_eq!(out.content["truncated_long_lines"], 1);
        let warnings = out.content["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning["kind"] == "long_line")
        );
    }

    #[tokio::test]
    async fn read_warns_on_cargo_fingerprint_noise() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("repo-scan.log");
        let mut body = String::new();
        for i in 0..10 {
            let _ = writeln!(
                body,
                " D target/debug/.fingerprint/package-{i}/dep-lib-package"
            );
        }
        tokio::fs::write(&path, &body).await.unwrap();

        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        let warnings = out.content["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning["kind"] == "rust_fingerprint_noise"),
            "expected fingerprint warning: {warnings:?}",
        );
    }

    #[tokio::test]
    async fn nonexistent_file_returns_is_error_true() {
        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": "/nonexistent/path/that/does/not/exist.txt" }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        assert!(out.is_error());
        assert_eq!(out.content["kind"], "io_error");
    }

    #[tokio::test]
    async fn on_success_registers_path_in_context() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("seen.txt");
        tokio::fs::write(&path, "hello\n").await.unwrap();

        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();
        tool.on_success(&out, &ctx).await;

        assert!(ctx.has_read_file(&path));
    }

    #[tokio::test]
    async fn on_success_does_not_register_io_error_paths() {
        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": "/no/such/path.txt" }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();
        tool.on_success(&out, &ctx).await;

        assert!(!ctx.has_read_file(Path::new("/no/such/path.txt")));
    }

    // --- Workspace confinement -------------------------------------------

    #[tokio::test]
    async fn confined_context_refuses_path_outside_root() {
        let dir = tempdir().unwrap();
        let tool = ReadTool::new();
        let mut ctx = ToolContext::empty();
        ctx.confine_to_workspace(dir.path().to_path_buf());
        ctx.set_working_dir(dir.path().to_path_buf());

        let envelope = envelope_for(json!({ "path": "/etc/passwd" }));
        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(out.is_error());
        assert_eq!(out.content["kind"], "confinement_refused");
        // The refused path must not be marked as read.
        tool.on_success(&out, &ctx).await;
        assert!(!ctx.has_read_file(Path::new("/etc/passwd")));
    }

    #[tokio::test]
    async fn confined_context_allows_path_inside_root() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("inside.txt");
        tokio::fs::write(&path, "ok\n").await.unwrap();
        let tool = ReadTool::new();
        let mut ctx = ToolContext::empty();
        ctx.confine_to_workspace(dir.path().to_path_buf());
        ctx.set_working_dir(dir.path().to_path_buf());

        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let out = tool.execute(&envelope, &ctx).await.unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["kind"], "text");
    }

    #[tokio::test]
    async fn read_tool_metadata() {
        let tool = ReadTool::new();
        assert_eq!(tool.name(), "read");
        assert_eq!(tool.effect(), ToolEffect::ReadOnly);
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "path");
    }
}
