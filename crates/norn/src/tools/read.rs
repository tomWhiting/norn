//! Read tool.
//!
//! Reads a UTF-8 text file from disk, returning its contents in `cat -n`
//! style (one-based line numbers, tab-separated). Detects binary files
//! and image extensions and reports them with a descriptive payload
//! instead of raw bytes. The file is streamed through the budget-bounded
//! scanner in `super::read_stream` — memory never scales with file
//! size, and the path is stat-ed before any content access. Successful
//! reads are recorded in `ToolContext` so the Write and Edit tools can
//! enforce read-before-modify.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;

use super::confinement::check_read_confinement;
use super::read_stream::{RenderedRead, ScannedFile, scan_file};
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::output_budget::ToolOutputBudget;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Image file extensions reported with the `image` kind instead of being read.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "svg", "webp"];

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
        // even metadata of out-of-root paths is never disclosed. Read-class
        // access additionally admits the operator-configured skill /
        // profile / config dirs (DECISIONS §0.6(b)).
        if let Err(reason) = check_read_confinement(ctx, &path) {
            return Ok(ToolOutput::failure_with_content(
                serde_json::json!({ "path": args.path, "kind": "confinement_refused" }),
                ToolErrorPayload::new(
                    ToolErrorKind::PermissionDenied,
                    format!("read refused: {reason}"),
                )
                .with_detail(serde_json::json!({ "path": args.path })),
            ));
        }

        // Stat before any content access: a missing path must fail here —
        // never fabricate a successful read (the image path previously
        // reported success for paths that did not exist).
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata,
            Err(e) => {
                return Ok(ToolOutput::failure_with_content(
                    serde_json::json!({ "path": args.path, "kind": "io_error" }),
                    ToolErrorPayload::new(ToolErrorKind::Io, e.to_string())
                        .with_detail(serde_json::json!({ "path": args.path })),
                ));
            }
        };

        // Image extension takes precedence over reading bytes; the stat
        // above guarantees the file actually exists.
        if is_image(&path) {
            let payload = serde_json::json!({
                "path": args.path,
                "kind": "image",
                "size_bytes": metadata.len(),
                "message": format!("image file: {}", args.path),
            });
            return Ok(ToolOutput::success(payload));
        }

        let budget = read_budget(ctx);
        let scanned = match scan_file(&path, args.offset, args.limit, budget).await {
            Ok(scanned) => scanned,
            Err(e) => {
                return Ok(ToolOutput::failure_with_content(
                    serde_json::json!({ "path": args.path, "kind": "io_error" }),
                    ToolErrorPayload::new(ToolErrorKind::Io, e.to_string())
                        .with_detail(serde_json::json!({ "path": args.path })),
                ));
            }
        };

        let rendered = match scanned {
            ScannedFile::Binary => {
                let payload = serde_json::json!({
                    "path": args.path,
                    "kind": "binary",
                    "size_bytes": metadata.len(),
                    "message": format!("binary file ({} bytes)", metadata.len()),
                });
                return Ok(ToolOutput::success(payload));
            }
            ScannedFile::NotUtf8 { message } => {
                let payload = serde_json::json!({
                    "path": args.path,
                    "kind": "binary",
                    "size_bytes": metadata.len(),
                    "message": message,
                });
                return Ok(ToolOutput::success(payload));
            }
            ScannedFile::Text(rendered) => rendered,
        };

        let warnings = read_warnings(&path, &rendered, budget);

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
            "file_size_bytes": metadata.len(),
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

fn read_budget(ctx: &ToolContext) -> ToolOutputBudget {
    ctx.get_extension::<ToolOutputBudget>()
        .map_or_else(ToolOutputBudget::default, |budget| *budget)
}

fn read_warnings(
    path: &Path,
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
    warnings.extend(build_artifact_warnings(path, rendered.fingerprint_hits));
    warnings
}

fn build_artifact_warnings(path: &Path, fingerprint_hits: usize) -> Vec<serde_json::Value> {
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
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::tool::envelope::ToolEnvelope;

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            model_args: args,
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

    /// Regression: the image path used to return success before any
    /// filesystem access, then `on_success` recorded the never-read path
    /// in the read-before-write set. A missing image must be an error and
    /// must never be marked read.
    #[tokio::test]
    async fn missing_image_path_is_an_error_and_not_marked_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ghost.png");

        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        assert!(out.is_error(), "missing image must be an io_error");
        assert_eq!(out.content["kind"], "io_error");
        tool.on_success(&out, &ctx).await;
        assert!(
            !ctx.has_read_file(&path),
            "a path that was never read must not enter the read set",
        );
    }

    #[tokio::test]
    async fn existing_image_reports_size_and_is_marked_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pic.png");
        tokio::fs::write(&path, b"fake-png-bytes").await.unwrap();

        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        assert!(!out.is_error());
        assert_eq!(out.content["kind"], "image");
        assert_eq!(out.content["size_bytes"], 14);
        tool.on_success(&out, &ctx).await;
        assert!(ctx.has_read_file(&path));
    }

    /// Regression: the whole file used to be loaded into memory before
    /// any budget applied. A file far larger than every configured budget
    /// must stream: the returned window stays budget-bounded while the
    /// totals stay exact.
    #[tokio::test]
    async fn oversized_file_returns_bounded_window_with_exact_totals() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("huge.log");
        let mut body = String::new();
        for i in 1..=50_000 {
            let _ = writeln!(body, "entry number {i} with some padding text");
        }
        tokio::fs::write(&path, &body).await.unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(ToolOutputBudget {
            read_default_line_limit: 50,
            read_hard_line_limit: 50,
            read_output_char_limit: 4_000,
            read_hard_output_char_limit: 4_000,
            read_line_char_limit: 200,
            model_output_inline_char_limit: 64_000,
        }));
        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["kind"], "text");
        assert_eq!(out.content["returned_lines"], 50);
        assert_eq!(out.content["total_lines"], 50_000);
        assert_eq!(out.content["file_size_bytes"], body.len());
        assert_eq!(out.content["truncated"], true);
        assert_eq!(out.content["next_offset"], 51);
        let content = out.content["content"].as_str().unwrap();
        assert!(content.chars().count() <= 4_000, "window stays in budget");

        // A window deep inside the oversized file is still reachable.
        let deep = tool
            .execute(
                &envelope_for(
                    json!({ "path": path.to_string_lossy(), "offset": 49_999, "limit": 2 }),
                ),
                &ctx,
            )
            .await
            .unwrap();
        let deep_content = deep.content["content"].as_str().unwrap();
        assert!(deep_content.contains("49999\tentry number 49999"));
        assert!(deep_content.contains("50000\tentry number 50000"));
    }

    #[tokio::test]
    async fn invalid_utf8_reports_binary_kind_with_offset() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mixed.txt");
        let mut bytes = b"good line\n".to_vec();
        bytes.extend_from_slice(&[0xC3, 0x28]); // invalid UTF-8 pair
        tokio::fs::write(&path, &bytes).await.unwrap();

        let tool = ReadTool::new();
        let envelope = envelope_for(json!({ "path": path.to_string_lossy() }));
        let ctx = ToolContext::empty();
        let out = tool.execute(&envelope, &ctx).await.unwrap();

        assert!(!out.is_error());
        assert_eq!(out.content["kind"], "binary");
        let message = out.content["message"].as_str().unwrap();
        assert!(message.contains("not valid UTF-8"), "{message}");
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

    /// DECISIONS §0.6(b): under confinement the read tool admits a file
    /// inside a declared read-exempt root (a home-level skill dir) even
    /// though it is outside the workspace root — the reported-but-unreadable
    /// companion-file bug is fixed. A non-exempt outside path stays refused.
    #[tokio::test]
    async fn confined_read_admits_exempt_skill_dir() {
        let outer = tempdir().unwrap();
        let root = outer.path().join("ws");
        let skills = outer.path().join("home-skills");
        tokio::fs::create_dir(&root).await.unwrap();
        tokio::fs::create_dir(&skills).await.unwrap();
        let companion = skills.join("SKILL.md");
        tokio::fs::write(&companion, "name: demo\n").await.unwrap();

        let tool = ReadTool::new();
        let mut ctx = ToolContext::empty();
        ctx.confine_to_workspace(root.clone());
        ctx.set_working_dir(root.clone());
        ctx.set_read_exempt_roots(vec![skills.clone()]);

        // The exempt skill companion is readable.
        let out = tool
            .execute(
                &envelope_for(json!({ "path": companion.to_string_lossy() })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(
            !out.is_error(),
            "exempt skill file must read: {:?}",
            out.content
        );
        assert_eq!(out.content["kind"], "text");

        // A non-exempt sibling outside the root is still refused.
        let secret = outer.path().join("secret.txt");
        tokio::fs::write(&secret, "s").await.unwrap();
        let refused = tool
            .execute(
                &envelope_for(json!({ "path": secret.to_string_lossy() })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(refused.is_error());
        assert_eq!(refused.content["kind"], "confinement_refused");
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
