//! `WebFetch` tool — streaming HTTP GET with HTML-to-markdown conversion.
//!
//! Fetches a URL with a hard 5 MiB size cap and a configurable timeout
//! (default 30s, max 120s). The response body is consumed via
//! `bytes_stream` so the cap is enforced incrementally without buffering
//! arbitrarily large responses; a body cut off at the cap is surfaced to
//! the model via the `truncated` flag and a `truncation_note` in the tool
//! result. HTML responses are converted to markdown using a parser-backed
//! HTML-to-Markdown converter. The converted page is archived under
//! `.norn/fetched/` resolved against the session's working directory
//! (through the tool context), never the process CWD.
//!
//! Every request passes the `super::ssrf` guard: private/internal
//! destinations (loopback, link-local/metadata, RFC 1918, IPv6 ULA) are
//! denied by default for literal and resolved hosts, and hostname
//! destinations are **pinned** — the connection is restricted to the
//! exact addresses the guard validated, closing the DNS-rebinding
//! window between validation and connect. Redirects are followed
//! *manually* so the guard re-validates (and re-pins) every hop; the
//! opt-out is [`WebFetchTool::allow_private_hosts`].
//!
//! `effect()` is [`ToolEffect::Network`] so the scheduler may run fetches
//! concurrently with other read-only / network tools.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::internal::extraction::{self, DetailLevel, SharedProvider};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
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

/// Maximum redirect hops followed manually (mirrors reqwest's own
/// default redirect limit). Each hop is re-validated by the SSRF guard.
const MAX_REDIRECT_HOPS: usize = 10;

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
///
/// The tool owns its HTTP client construction: the redirect policy MUST
/// be `none` (redirects are followed manually so the SSRF guard
/// re-validates every hop), and a built `reqwest::Client` cannot be
/// inspected to verify that — so no pre-built client is accepted.
pub struct WebFetchTool {
    /// Client used for unpinned (literal-IP / opt-out) requests.
    client: reqwest::Client,
    /// SSRF-guard opt-out; see [`Self::allow_private_hosts`].
    allow_private_hosts: bool,
}

/// Builds the tool's HTTP client: fixed user agent, redirects disabled
/// (followed manually for per-hop SSRF validation), and — when `pin` is
/// supplied — DNS resolution for that hostname overridden to the exact
/// addresses the SSRF guard validated, so the checked address is the
/// connected address.
fn build_http_client(
    pin: Option<(&str, &[std::net::SocketAddr])>,
) -> Result<reqwest::Client, ToolError> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none());
    if let Some((domain, addrs)) = pin {
        builder = builder.resolve_to_addrs(domain, addrs);
    }
    builder.build().map_err(|e| ToolError::ExecutionFailed {
        reason: format!("failed to build HTTP client: {e}"),
    })
}

impl WebFetchTool {
    /// Creates a `WebFetchTool`.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be built.
    pub fn new() -> Result<Self, ToolError> {
        Ok(Self {
            client: build_http_client(None)?,
            allow_private_hosts: false,
        })
    }

    /// Explicit opt-out from the SSRF guard's default-deny of
    /// private/internal destinations (loopback, link-local/metadata,
    /// RFC 1918, IPv6 unique-local). Intended for embedders that
    /// intentionally fetch from hosts on their own network.
    #[must_use]
    pub fn allow_private_hosts(mut self, allow: bool) -> Self {
        self.allow_private_hosts = allow;
        self
    }

    /// Performs the GET for `url`, following up to [`MAX_REDIRECT_HOPS`]
    /// redirects manually. Every hop — including the initial URL — is
    /// validated by the SSRF guard, restricted to http(s), and (for
    /// hostname destinations) connected through a client pinned to the
    /// validated addresses.
    async fn fetch_following_redirects(
        &self,
        mut current: url::Url,
        timeout: Duration,
    ) -> Result<reqwest::Response, ToolError> {
        for _hop in 0..=MAX_REDIRECT_HOPS {
            let validation =
                super::ssrf::validate_url_host(&current, self.allow_private_hosts).await?;
            let client = match &validation {
                super::ssrf::HostValidation::Unpinned => self.client.clone(),
                super::ssrf::HostValidation::Pinned { domain, addrs } => {
                    build_http_client(Some((domain.as_str(), addrs.as_slice())))?
                }
            };

            let response = client
                .get(current.clone())
                .header(reqwest::header::USER_AGENT, USER_AGENT)
                .timeout(timeout)
                .send()
                .await
                .map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("HTTP GET failed: {e}"),
                })?;

            if !response.status().is_redirection() {
                return Ok(response);
            }

            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| ToolError::ExecutionFailed {
                    reason: format!(
                        "HTTP {} from {current} carried no Location header",
                        response.status()
                    ),
                })?
                .to_str()
                .map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("redirect Location from {current} is not valid text: {e}"),
                })?;
            let next = current
                .join(location)
                .map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("invalid redirect target `{location}` from {current}: {e}"),
                })?;
            if !matches!(next.scheme(), "http" | "https") {
                return Err(ToolError::ExecutionFailed {
                    reason: format!("redirect from {current} to non-http(s) URL {next} refused"),
                });
            }
            current = next;
        }
        Err(ToolError::ExecutionFailed {
            reason: format!("too many redirects (limit {MAX_REDIRECT_HOPS})"),
        })
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
        let args: WebFetchArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::pre_validation(
                    ToolErrorKind::InvalidArguments,
                    format!("invalid web_fetch arguments: {e}"),
                )
            })?;

        if !is_http_url(&args.url) {
            return Err(ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                "url must start with http:// or https://",
            ));
        }
        let parsed_url = url::Url::parse(&args.url).map_err(|e| {
            ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                format!("invalid url `{}`: {e}", args.url),
            )
        })?;

        let format = parse_format(args.format.as_deref())?;
        let timeout_secs = args.timeout.unwrap_or(DEFAULT_TIMEOUT).min(MAX_TIMEOUT);
        let extraction_detail = parse_detail(args.detail.as_deref())?;

        let response = self
            .fetch_following_redirects(parsed_url, Duration::from_secs(timeout_secs))
            .await?;

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

        let (body_bytes, truncated) = read_body_with_cap(response).await?;
        let body = String::from_utf8_lossy(&body_bytes).into_owned();
        let converted = convert(&body, format, &content_type);

        let saved_path = save_fetched_content(ctx, &args.url, &converted).await?;

        let questions = args.questions;

        let provider = ctx.require_extension::<SharedProvider>()?;

        let line_count = converted.lines().count();
        let mut page_fields = json!({
            "url": args.url,
            "content_type": content_type,
            "line_count": line_count,
            "truncated": truncated,
            "saved_to": saved_path.to_string_lossy(),
        });
        if truncated && let Some(map) = page_fields.as_object_mut() {
            map.insert(
                "truncation_note".to_owned(),
                Value::String(format!(
                    "The response body exceeded the {MAX_SIZE}-byte cap and was \
                     truncated; the answers and the saved file reflect only the \
                     first {MAX_SIZE} bytes of the page."
                )),
            );
        }

        let numbered = prepend_line_numbers(&converted);
        let answers = match extraction::extract(
            provider.0.as_ref(),
            &numbered,
            &questions,
            extraction_detail,
        )
        .await
        {
            Ok(answers) => answers,
            // The page was fetched and archived successfully; an
            // extraction failure must not discard that work. The model
            // gets the archive location alongside the typed error so it
            // can read the saved page or retry the questions.
            Err(e) => {
                return Ok(ToolOutput::failure_with_content(
                    page_fields,
                    ToolErrorPayload::new(
                        ToolErrorKind::ExecutionFailed,
                        format!("page fetched and archived, but answer extraction failed: {e}"),
                    )
                    .with_detail(json!({
                        "saved_to": saved_path.to_string_lossy(),
                        "url": args.url,
                    })),
                ));
            }
        };

        if let Some(map) = page_fields.as_object_mut() {
            map.insert("answers".to_owned(), answers);
        }
        Ok(ToolOutput::success(page_fields))
    }
}

/// Output format selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Markdown,
    Text,
    Html,
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn parse_format(raw: Option<&str>) -> Result<Format, ToolError> {
    match raw {
        None | Some("markdown") => Ok(Format::Markdown),
        Some("text") => Ok(Format::Text),
        Some("html") => Ok(Format::Html),
        Some(other) => Err(ToolError::pre_validation(
            ToolErrorKind::InvalidArguments,
            format!("unknown format `{other}`; expected markdown|text|html"),
        )),
    }
}

fn parse_detail(raw: Option<&str>) -> Result<DetailLevel, ToolError> {
    match raw {
        None | Some("normal") => Ok(DetailLevel::Normal),
        Some("brief") => Ok(DetailLevel::Brief),
        Some("detailed") => Ok(DetailLevel::Detailed),
        Some(other) => Err(ToolError::pre_validation(
            ToolErrorKind::InvalidArguments,
            format!("unknown detail `{other}`; expected brief|normal|detailed"),
        )),
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
    convert_html(html, html_to_markdown_rs::OutputFormat::Plain, "text")
}

fn html_to_markdown(html: &str) -> String {
    convert_html(
        html,
        html_to_markdown_rs::OutputFormat::Markdown,
        "markdown",
    )
}

/// Convert HTML via the parser-backed converter. When conversion fails
/// (or produces no content) the raw HTML is returned so the page is
/// never lost — but the fallback is logged, never silent: raw HTML in a
/// `markdown`/`text` result is a degraded outcome the operator must be
/// able to trace.
fn convert_html(html: &str, format: html_to_markdown_rs::OutputFormat, label: &str) -> String {
    match html_to_markdown_rs::convert(html, Some(html_conversion_options(format))) {
        Ok(result) => {
            let Some(content) = result.content else {
                tracing::warn!(
                    target_format = label,
                    "HTML conversion produced no content; returning raw HTML",
                );
                return html.to_owned();
            };
            content
        }
        Err(error) => {
            tracing::warn!(
                target_format = label,
                error = ?error,
                "HTML conversion failed; returning raw HTML",
            );
            html.to_owned()
        }
    }
}

/// Archive the converted page under `.norn/fetched/` resolved against the
/// session's working directory via [`ToolContext::resolve_path`] — never the
/// process CWD — using async filesystem calls so the executor is not blocked.
async fn save_fetched_content(
    ctx: &ToolContext,
    url: &str,
    content: &str,
) -> Result<PathBuf, ToolError> {
    let dir = ctx.resolve_path(PathBuf::from(".norn").join("fetched"));
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("failed to create {}: {e}", dir.display()),
        })?;
    // SHA-256 of the URL: stable across processes and Rust releases, so
    // re-fetching a page always lands on the same archive file
    // (`DefaultHasher` guarantees neither).
    let hash = {
        use sha2::{Digest as _, Sha256};
        format!("{:x}", Sha256::digest(url.as_bytes()))
    };
    let filename = format!("{hash}.md");
    let path = dir.join(&filename);
    let body = strip_frontmatter(content);
    let with_frontmatter = format!(
        "---\nurl: {url}\nfetched: {}\n---\n\n{body}",
        chrono::Utc::now().to_rfc3339()
    );
    tokio::fs::write(&path, &with_frontmatter)
        .await
        .map_err(|e| ToolError::ExecutionFailed {
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

        // The mock server is loopback, so the SSRF opt-out is required to
        // reach it; the fetch then succeeds and the missing SharedProvider
        // is what fails.
        let tool = WebFetchTool::new()
            .expect("client builds")
            .allow_private_hosts(true);
        let env = envelope(json!({
            "url": format!("{}/page", server.uri()),
            "questions": ["What is the answer?"],
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("questions require SharedProvider");
        assert!(
            err.to_string().contains("SharedProvider"),
            "fetch must have succeeded and failed on the provider: {err}"
        );
    }

    // --- Artifact placement and truncation surfacing -------------------------

    use std::path::Path;
    use std::sync::Arc;

    use crate::provider::events::StopReason;
    use crate::provider::mock::MockProvider;
    use crate::provider::usage::Usage;
    use crate::tool::context::SharedWorkingDir;

    /// A context whose working dir is `dir` and whose `SharedProvider` is a
    /// mock extraction agent returning one fixed answer set.
    fn ctx_with_extraction_provider(dir: &Path) -> ToolContext {
        let ctx = ToolContext::with_working_dir(SharedWorkingDir::new(dir.to_path_buf()));
        let provider: Arc<dyn crate::provider::traits::Provider> =
            Arc::new(MockProvider::new(vec![vec![
                crate::provider::events::ProviderEvent::TextDelta {
                    text: r#"[{"question":"summary","answer":"ok","lines":"1"}]"#.to_owned(),
                },
                crate::provider::events::ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                },
            ]]));
        ctx.insert_extension(Arc::new(SharedProvider(provider)));
        ctx
    }

    /// Track B finding 6 regression: the `.norn/fetched` artifact must land
    /// under the session's working directory (resolved through the tool
    /// context), not wherever the process happened to start, and an
    /// untruncated fetch must say so explicitly.
    #[tokio::test]
    async fn fetch_artifact_saves_under_context_working_dir() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string("<h1>Title</h1><p>Body text.</p>"),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_with_extraction_provider(dir.path());
        let tool = WebFetchTool::new()
            .expect("client builds")
            .allow_private_hosts(true);
        let out = tool
            .execute(
                &envelope(json!({ "url": format!("{}/page", server.uri()) })),
                &ctx,
            )
            .await
            .expect("fetch succeeds");

        let saved = out.content["saved_to"].as_str().expect("saved_to string");
        let saved_path = Path::new(saved);
        assert!(
            saved_path.starts_with(dir.path()),
            "artifact must land under the context working dir; saved_to = {saved}",
        );
        assert!(saved_path.exists(), "artifact file must exist at {saved}");
        assert_eq!(
            out.content["truncated"],
            json!(false),
            "an untruncated fetch must carry truncated=false",
        );
        assert!(
            out.content.get("truncation_note").is_none(),
            "no truncation note for an untruncated body",
        );
    }

    /// Track B finding 6 regression: a body that exceeds the size cap must
    /// be flagged as truncated in the tool result — the model must never be
    /// shown a truncated page as if it were complete.
    ///
    /// A declared `Content-Length` over the cap is rejected outright before
    /// streaming, so this test serves the oversized body over a raw socket
    /// *without* a `Content-Length` header (connection-close delimited) to
    /// exercise the incremental cap genuinely.
    #[tokio::test]
    async fn truncated_body_is_flagged_in_result() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept connection");
            // Read the request head; we only ever serve one canned response.
            let mut buf = vec![0u8; 8192];
            let _ = socket.read(&mut buf).await.expect("read request");
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\n",
                )
                .await
                .expect("write headers");
            let chunk = vec![b'a'; 64 * 1024];
            let mut written = 0usize;
            while written < MAX_SIZE + 64 * 1024 {
                // The client deliberately stops reading once its size cap is
                // hit, so a write error here just means it hung up — done.
                if socket.write_all(&chunk).await.is_err() {
                    return;
                }
                written += chunk.len();
            }
            let _ = socket.shutdown().await;
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_with_extraction_provider(dir.path());
        let tool = WebFetchTool::new()
            .expect("client builds")
            .allow_private_hosts(true);
        let out = tool
            .execute(
                &envelope(json!({ "url": format!("http://{addr}/big") })),
                &ctx,
            )
            .await
            .expect("fetch succeeds with a capped body");
        server.abort();

        assert_eq!(
            out.content["truncated"],
            json!(true),
            "an over-cap body must be flagged truncated; content: {}",
            out.content,
        );
        let note = out.content["truncation_note"]
            .as_str()
            .expect("truncation note present for a truncated body");
        assert!(note.contains("truncated"), "{note}");
    }

    /// Archive filenames must be the SHA-256 of the URL: stable across
    /// processes and Rust releases (`DefaultHasher` is neither), so the
    /// same page always maps to the same archive file.
    #[tokio::test]
    async fn archive_filename_is_stable_sha256_of_url() {
        use sha2::{Digest as _, Sha256};

        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::with_working_dir(SharedWorkingDir::new(dir.path().to_path_buf()));
        let url = "https://example.com/some/page?q=1";

        let first = save_fetched_content(&ctx, url, "converted body")
            .await
            .expect("save succeeds");
        let second = save_fetched_content(&ctx, url, "converted body again")
            .await
            .expect("save succeeds");
        assert_eq!(first, second, "same URL must map to the same archive file");

        let expected = format!("{:x}.md", Sha256::digest(url.as_bytes()));
        assert_eq!(
            first.file_name().and_then(|n| n.to_str()),
            Some(expected.as_str()),
        );
    }

    /// Extraction failure must not discard the archived page: the result
    /// is a typed failure that still carries `saved_to` (and the archive
    /// file exists) so the model can read the page or retry.
    #[tokio::test]
    async fn extraction_failure_preserves_archived_page() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string("<h1>Title</h1><p>Body.</p>"),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        // Provider whose response is not the JSON array the extraction
        // agent requires — extract() fails after the page was archived.
        let ctx = ToolContext::with_working_dir(SharedWorkingDir::new(dir.path().to_path_buf()));
        let provider: Arc<dyn crate::provider::traits::Provider> =
            Arc::new(MockProvider::new(vec![vec![
                crate::provider::events::ProviderEvent::TextDelta {
                    text: "this is not json".to_owned(),
                },
                crate::provider::events::ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                },
            ]]));
        ctx.insert_extension(Arc::new(SharedProvider(provider)));

        let tool = WebFetchTool::new()
            .expect("client builds")
            .allow_private_hosts(true);
        let out = tool
            .execute(
                &envelope(json!({ "url": format!("{}/page", server.uri()) })),
                &ctx,
            )
            .await
            .expect("fetch itself succeeds");

        assert!(out.is_error(), "extraction failure is a tool failure");
        let saved = out.content["saved_to"]
            .as_str()
            .expect("saved_to survives extraction failure");
        assert!(Path::new(saved).exists(), "archived page exists at {saved}");
        let error_message = out.content["error"]["message"].as_str().unwrap_or_default();
        assert!(
            error_message.contains("archived"),
            "error must tell the model the page was preserved: {error_message}",
        );
    }

    /// DNS-pinning wiring: a client pinned to validated addresses
    /// connects to exactly those addresses regardless of what the name
    /// would resolve to (here, a name that does not resolve at all).
    #[tokio::test]
    async fn pinned_client_connects_to_validated_addrs() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pinned"))
            .respond_with(ResponseTemplate::new(200).set_body_string("pinned ok"))
            .mount(&server)
            .await;
        let addr: std::net::SocketAddr = server.address().to_owned();

        let client = build_http_client(Some(("pinned-host.invalid", &[addr])))
            .expect("pinned client builds");
        let response = client
            .get(format!("http://pinned-host.invalid:{}/pinned", addr.port()))
            .send()
            .await
            .expect("pinned request must bypass DNS and hit the validated addr");
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.expect("body"), "pinned ok");
    }

    // --- SSRF guard ---------------------------------------------------------

    #[tokio::test]
    async fn execute_refuses_loopback_by_default() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string("internal"))
            .mount(&server)
            .await;

        let tool = WebFetchTool::new().expect("client builds");
        let env = envelope(json!({ "url": format!("{}/secret", server.uri()) }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("loopback must be denied by default");
        let msg = err.to_string();
        assert!(msg.contains("SSRF"), "{msg}");
        assert!(msg.contains("loopback"), "{msg}");
    }

    #[tokio::test]
    async fn redirect_hops_are_validated_for_scheme() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/jump"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "ftp://example.com/payload"),
            )
            .mount(&server)
            .await;

        let tool = WebFetchTool::new()
            .expect("client builds")
            .allow_private_hosts(true);
        let env = envelope(json!({ "url": format!("{}/jump", server.uri()) }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("non-http(s) redirect must be refused");
        assert!(err.to_string().contains("non-http(s)"), "{err}");
    }

    #[tokio::test]
    async fn redirect_loop_is_bounded() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let loop_url = format!("{}/loop", server.uri());
        Mock::given(method("GET"))
            .and(path("/loop"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", loop_url.as_str()))
            .mount(&server)
            .await;

        let tool = WebFetchTool::new()
            .expect("client builds")
            .allow_private_hosts(true);
        let env = envelope(json!({ "url": loop_url }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("redirect loop must terminate");
        assert!(err.to_string().contains("too many redirects"), "{err}");
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
