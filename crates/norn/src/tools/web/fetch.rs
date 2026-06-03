//! `WebFetch` tool — streaming HTTP GET with HTML-to-markdown conversion.
//!
//! Fetches a URL with a hard 5 MiB size cap and a configurable timeout
//! (default 30s, max 120s). The response body is consumed via
//! `bytes_stream` so the cap is enforced incrementally without buffering
//! arbitrarily large responses. HTML responses are converted to markdown
//! using a parser-backed HTML-to-Markdown converter.
//!
//! `effect()` is [`ToolEffect::Network`] so the scheduler may run fetches
//! concurrently with other read-only / network tools.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::internal::extraction::{self, DetailLevel, SharedProvider};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Hard cap on response body size (bytes).
const MAX_SIZE: usize = 5 * 1024 * 1024;

/// Default request timeout (seconds) when the caller does not specify.
const DEFAULT_TIMEOUT: u64 = 30;

/// Maximum allowed request timeout (seconds).
const MAX_TIMEOUT: u64 = 120;

/// User-Agent header sent with every fetch.
const USER_AGENT: &str = "Mozilla/5.0 (compatible; Norn/1.0)";

/// Model-supplied arguments for [`WebFetchTool`].
#[derive(Debug, Deserialize)]
struct WebFetchArgs {
    /// URL to fetch. Must start with `http://` or `https://`.
    url: String,
    /// Output format: `markdown` (default), `text`, or `html`.
    #[serde(default)]
    format: Option<String>,
    /// Request timeout in seconds. Defaults to 30, capped at 120.
    #[serde(default)]
    timeout: Option<u64>,
    /// Questions to answer from the page content using the extraction agent.
    #[serde(default)]
    questions: Vec<String>,
    /// Detail level for extraction answers: `brief`, `normal`, or `detailed`.
    #[serde(default)]
    detail: Option<String>,
}

/// `WebFetch` tool: bounded streaming HTTP GET with HTML-to-markdown.
pub struct WebFetchTool {
    client: reqwest::Client,
}

impl WebFetchTool {
    /// Creates a `WebFetchTool` with a fresh `reqwest::Client`.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be built.
    pub fn new() -> Result<Self, ToolError> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("failed to build HTTP client: {e}"),
            })?;
        Ok(Self { client })
    }

    /// Constructs a `WebFetchTool` from a pre-built `reqwest::Client`.
    #[must_use]
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &'static str {
        "web_fetch"
    }

    fn description(&self) -> &'static str {
        include_str!("../guidance/web_fetch.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Web
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../guidance/web_fetch.usage.md"))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to fetch. Must start with http:// or https://."
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "text", "html"],
                    "description": "Output format. Defaults to markdown."
                },
                "timeout": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 120,
                    "description": "Request timeout in seconds. Defaults to 30, capped at 120."
                },
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "description": "Questions to answer from the page content. The page is read by an extraction agent and only the answers are returned. If omitted, the agent summarises the key information from the page."
                },
                "detail": {
                    "type": "string",
                    "enum": ["brief", "normal", "detailed"],
                    "default": "normal",
                    "description": "Level of detail for extraction answers. Only used when questions is provided."
                }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Network
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let started = Instant::now();
        let args: WebFetchArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::PreValidationFailed {
                    reason: format!("invalid web_fetch arguments: {e}"),
                }
            })?;

        if !is_http_url(&args.url) {
            return Err(ToolError::PreValidationFailed {
                reason: "url must start with http:// or https://".to_owned(),
            });
        }

        let format = parse_format(args.format.as_deref())?;
        let timeout_secs = args.timeout.unwrap_or(DEFAULT_TIMEOUT).min(MAX_TIMEOUT);
        let extraction_detail = parse_detail(args.detail.as_deref())?;

        let response = self
            .client
            .get(&args.url)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .timeout(Duration::from_secs(timeout_secs))
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("HTTP GET failed: {e}"),
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed {
                reason: format!("HTTP {status}"),
            });
        }

        if let Some(len) = response.content_length()
            && usize::try_from(len).unwrap_or(usize::MAX) > MAX_SIZE
        {
            return Err(ToolError::ExecutionFailed {
                reason: format!("response Content-Length {len} exceeds {MAX_SIZE}-byte cap"),
            });
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        let (body_bytes, _truncated) = read_body_with_cap(response).await?;
        let body = String::from_utf8_lossy(&body_bytes).into_owned();
        let converted = convert(&body, format, &content_type);

        let saved_path = save_fetched_content(&args.url, &converted)?;

        let questions = args.questions;

        let provider =
            ctx.get_extension::<SharedProvider>()
                .ok_or_else(|| ToolError::ExecutionFailed {
                    reason:
                        "web_fetch extraction is not available: SharedProvider extension is missing"
                            .to_owned(),
                })?;

        let numbered = prepend_line_numbers(&converted);
        let answers = extraction::extract(
            provider.0.as_ref(),
            &numbered,
            &questions,
            extraction_detail,
        )
        .await?;

        let line_count = converted.lines().count();

        Ok(ToolOutput {
            content: json!({
                "url": args.url,
                "content_type": content_type,
                "line_count": line_count,
                "answers": answers,
                "saved_to": saved_path.to_string_lossy(),
            }),
            is_error: false,
            duration: started.elapsed(),
        })
    }
}

/// Output format selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Markdown,
    Text,
    Html,
}

impl Format {}

fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn parse_format(raw: Option<&str>) -> Result<Format, ToolError> {
    match raw {
        None | Some("markdown") => Ok(Format::Markdown),
        Some("text") => Ok(Format::Text),
        Some("html") => Ok(Format::Html),
        Some(other) => Err(ToolError::PreValidationFailed {
            reason: format!("unknown format `{other}`; expected markdown|text|html"),
        }),
    }
}

fn parse_detail(raw: Option<&str>) -> Result<DetailLevel, ToolError> {
    match raw {
        None | Some("normal") => Ok(DetailLevel::Normal),
        Some("brief") => Ok(DetailLevel::Brief),
        Some("detailed") => Ok(DetailLevel::Detailed),
        Some(other) => Err(ToolError::PreValidationFailed {
            reason: format!("unknown detail `{other}`; expected brief|normal|detailed"),
        }),
    }
}

async fn read_body_with_cap(response: reqwest::Response) -> Result<(Vec<u8>, bool), ToolError> {
    let mut body_bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ToolError::ExecutionFailed {
            reason: format!("response body read failed: {e}"),
        })?;
        let remaining = MAX_SIZE.saturating_sub(body_bytes.len());
        if chunk.len() > remaining {
            body_bytes.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        body_bytes.extend_from_slice(&chunk);
    }
    Ok((body_bytes, truncated))
}

fn convert(body: &str, format: Format, content_type: &str) -> String {
    match format {
        Format::Html => body.to_owned(),
        Format::Text => html_to_text(body),
        Format::Markdown => {
            if content_type.contains("text/html") {
                html_to_markdown(body)
            } else {
                body.to_owned()
            }
        }
    }
}

fn html_conversion_options(
    output_format: html_to_markdown_rs::OutputFormat,
) -> html_to_markdown_rs::ConversionOptions {
    html_to_markdown_rs::ConversionOptions::builder()
        .preprocessing(html_to_markdown_rs::PreprocessingOptions {
            enabled: true,
            preset: html_to_markdown_rs::PreprocessingPreset::Aggressive,
            remove_navigation: true,
            remove_forms: true,
        })
        .compact_tables(true)
        .output_format(output_format)
        .build()
}

fn html_to_text(html: &str) -> String {
    html_to_markdown_rs::convert(
        html,
        Some(html_conversion_options(
            html_to_markdown_rs::OutputFormat::Plain,
        )),
    )
    .ok()
    .and_then(|result| result.content)
    .unwrap_or_else(|| html.to_owned())
}

fn html_to_markdown(html: &str) -> String {
    html_to_markdown_rs::convert(
        html,
        Some(html_conversion_options(
            html_to_markdown_rs::OutputFormat::Markdown,
        )),
    )
    .ok()
    .and_then(|result| result.content)
    .unwrap_or_else(|| html.to_owned())
}

fn save_fetched_content(url: &str, content: &str) -> Result<PathBuf, ToolError> {
    let dir = PathBuf::from(".norn/fetched");
    std::fs::create_dir_all(&dir).map_err(|e| ToolError::ExecutionFailed {
        reason: format!("failed to create .norn/fetched/: {e}"),
    })?;
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        url.hash(&mut h);
        format!("{:016x}", h.finish())
    };
    let filename = format!("{hash}.md");
    let path = dir.join(&filename);
    let body = strip_frontmatter(content);
    let with_frontmatter = format!(
        "---\nurl: {url}\nfetched: {}\n---\n\n{body}",
        chrono::Utc::now().to_rfc3339()
    );
    std::fs::write(&path, &with_frontmatter).map_err(|e| ToolError::ExecutionFailed {
        reason: format!("failed to write fetched content to {}: {e}", path.display()),
    })?;
    Ok(path)
}

fn strip_frontmatter(content: &str) -> &str {
    if !content.starts_with("---") {
        return content;
    }
    if let Some(end) = content[3..].find("\n---") {
        let after = 3 + end + 4;
        content[after..].trim_start_matches('\n')
    } else {
        content
    }
}

fn prepend_line_numbers(content: &str) -> String {
    let mut out = String::with_capacity(content.len() + content.lines().count() * 6);
    for (i, line) in content.lines().enumerate() {
        let _ = writeln!(out, "{}\t{}", i + 1, line);
    }
    out
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
    use super::*;
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};

    fn envelope(args: Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_owned(),
            tool_name: "web_fetch".to_owned(),
            model_args: args,
            runtime_inputs: RuntimeInputs::default(),
            metadata: Value::Null,
        }
    }

    #[test]
    fn tool_object_safe() {
        let tool = WebFetchTool::new().expect("client builds");
        let _: Box<dyn Tool + Send + Sync> = Box::new(tool);
    }

    #[test]
    fn name_and_effect() {
        let tool = WebFetchTool::new().expect("client builds");
        assert_eq!(tool.name(), "web_fetch");
        assert_eq!(tool.effect(), ToolEffect::Network);
    }

    #[test]
    fn input_schema_declares_url_required() {
        let tool = WebFetchTool::new().expect("client builds");
        let schema = tool.input_schema();
        let required = schema["required"].as_array().expect("required array");
        assert!(required.iter().any(|v| v == "url"));
        let enum_values = schema["properties"]["format"]["enum"]
            .as_array()
            .expect("format enum");
        let names: Vec<&str> = enum_values.iter().filter_map(Value::as_str).collect();
        assert!(names.contains(&"markdown"));
        assert!(names.contains(&"text"));
        assert!(names.contains(&"html"));
        let questions = &schema["properties"]["questions"];
        assert_eq!(questions["type"], "array");
        assert_eq!(questions["items"]["type"], "string");
        let detail_enum = schema["properties"]["detail"]["enum"]
            .as_array()
            .expect("detail enum");
        let detail_names: Vec<&str> = detail_enum.iter().filter_map(Value::as_str).collect();
        assert!(detail_names.contains(&"brief"));
        assert!(detail_names.contains(&"normal"));
        assert!(detail_names.contains(&"detailed"));
        assert_eq!(schema["properties"]["detail"]["default"], "normal");
        assert_eq!(schema["additionalProperties"], false);
    }

    #[tokio::test]
    async fn execute_rejects_non_http_scheme() {
        let tool = WebFetchTool::new().expect("client builds");
        let env = envelope(json!({ "url": "ftp://example.com/file" }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("non-http must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn execute_rejects_unknown_format() {
        let tool = WebFetchTool::new().expect("client builds");
        let env = envelope(json!({
            "url": "https://example.com/",
            "format": "yaml",
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("unknown format must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn execute_rejects_unknown_detail() {
        let tool = WebFetchTool::new().expect("client builds");
        let env = envelope(json!({
            "url": "https://example.com/",
            "detail": "verbose",
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("unknown detail must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn execute_with_questions_requires_shared_provider() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string("<h1>Title</h1><p>Answer is here.</p>"),
            )
            .mount(&server)
            .await;

        let tool = WebFetchTool::new().expect("client builds");
        let env = envelope(json!({
            "url": format!("{}/page", server.uri()),
            "questions": ["What is the answer?"],
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("questions require SharedProvider");
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn html_to_markdown_handles_headings() {
        let html = "<h1>Title</h1><h2>Section</h2><p>Body</p>";
        let md = html_to_markdown(html);
        assert!(md.contains("# Title"));
        assert!(md.contains("## Section"));
        assert!(md.contains("Body"));
    }

    #[test]
    fn html_to_markdown_handles_links_bold_italic() {
        let html =
            r#"<a href="https://example.com/">Example</a> <strong>bold</strong> <em>it</em>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("[Example](https://example.com/)"));
        assert!(md.contains("**bold**"));
        assert!(md.contains("*it*"));
    }

    #[test]
    fn html_to_markdown_handles_code_blocks() {
        let html = "<pre><code>fn main() {}</code></pre>";
        let md = html_to_markdown(html);
        assert!(md.contains("```"));
        assert!(md.contains("fn main() {}"));
    }

    #[test]
    fn html_to_markdown_handles_lists() {
        let html = "<ul><li>one</li><li>two</li></ul>";
        let md = html_to_markdown(html);
        assert!(md.contains("- one"));
        assert!(md.contains("- two"));
    }

    #[test]
    fn html_to_markdown_strips_script_and_style() {
        let html = "<script>evil()</script><style>body{}</style><p>safe</p>";
        let md = html_to_markdown(html);
        assert!(!md.contains("evil"));
        assert!(!md.contains("body{}"));
        assert!(md.contains("safe"));
    }

    #[test]
    fn html_to_text_strips_all_tags() {
        let html = "<div><p>one</p><p>two</p></div>";
        let text = html_to_text(html);
        assert!(text.contains("one"));
        assert!(text.contains("two"));
        assert!(!text.contains('<'));
    }

    #[test]
    fn html_to_markdown_handles_tables() {
        let html =
            "<table><tr><th>Name</th><th>Value</th></tr><tr><td>one</td><td>1</td></tr></table>";
        let md = html_to_markdown(html);
        assert!(md.contains("| Name | Value |"));
        assert!(md.contains("| one | 1 |"));
    }

    #[test]
    fn html_to_text_decodes_entities() {
        let text = html_to_text("<p>don&apos;t &amp; stop</p>");
        assert!(text.contains("don't & stop"));
        assert!(!text.contains("&amp;"));
    }

    #[test]
    fn is_http_url_accepts_http_and_https() {
        assert!(is_http_url("http://example.com"));
        assert!(is_http_url("https://example.com"));
        assert!(!is_http_url("ftp://example.com"));
        assert!(!is_http_url("file:///tmp/x"));
        assert!(!is_http_url(""));
    }

    #[test]
    fn parse_format_defaults_to_markdown() {
        assert_eq!(parse_format(None).expect("default ok"), Format::Markdown);
        assert_eq!(
            parse_format(Some("markdown")).expect("markdown ok"),
            Format::Markdown
        );
        assert_eq!(parse_format(Some("text")).expect("text ok"), Format::Text);
        assert_eq!(parse_format(Some("html")).expect("html ok"), Format::Html);
        assert!(parse_format(Some("yaml")).is_err());
    }

    #[test]
    fn parse_detail_defaults_to_normal() {
        assert_eq!(parse_detail(None).expect("default ok"), DetailLevel::Normal);
        assert_eq!(
            parse_detail(Some("brief")).expect("brief ok"),
            DetailLevel::Brief
        );
        assert_eq!(
            parse_detail(Some("normal")).expect("normal ok"),
            DetailLevel::Normal
        );
        assert_eq!(
            parse_detail(Some("detailed")).expect("detailed ok"),
            DetailLevel::Detailed
        );
        assert!(parse_detail(Some("verbose")).is_err());
    }
}
