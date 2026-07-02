//! `WebSearch` tool.
//!
//! Providers that support hosted web search can project this runtime tool into
//! their native hosted-tool surface. Providers without that capability keep the
//! local fallback, which scrapes `DuckDuckGo`'s HTML endpoint and returns
//! parsed results.
//!
//! `effect()` is [`ToolEffect::Network`] so the scheduler may run web
//! searches concurrently with other read-only / network tools.

use std::fmt::Write as _;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::ToolErrorKind;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Runtime tool name for web search.
pub const WEB_SEARCH_TOOL_NAME: &str = "web_search";

/// Default number of results when the model does not specify.
const DEFAULT_NUM_RESULTS: u32 = 8;

/// Hard cap on results regardless of what the model requests.
const MAX_NUM_RESULTS: u32 = 20;

/// Duration applied to the DDG fallback HTTP request.
const FALLBACK_TIMEOUT: Duration = Duration::from_secs(20);

/// Model-supplied arguments for [`WebSearchTool`].
#[derive(Debug, Deserialize, Serialize)]
struct WebSearchArgs {
    /// Search query string.
    query: String,
    /// Maximum number of results to return. Defaults to 8, capped at 20.
    #[serde(default)]
    num_results: Option<u32>,
}

/// A single parsed search result.
#[derive(Debug, Serialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// `WebSearch` tool: `DuckDuckGo` HTML scraping fallback for `web_search`.
pub struct WebSearchTool {
    client: reqwest::Client,
}

impl WebSearchTool {
    /// Creates a `WebSearchTool` with a fresh `reqwest::Client`.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be built.
    pub fn new() -> Result<Self, ToolError> {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
            .build()
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("failed to build HTTP client: {e}"),
            })?;
        Ok(Self { client })
    }

    /// Constructs a `WebSearchTool` from a pre-built `reqwest::Client`.
    #[must_use]
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        WEB_SEARCH_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        include_str!("../guidance/web_search.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Web
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../guidance/web_search.usage.md"))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query string."
                },
                "num_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "description": "Maximum results to return. Defaults to 8, capped at 20."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Network
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: WebSearchArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::pre_validation(
                    ToolErrorKind::InvalidArguments,
                    format!("invalid web_search arguments: {e}"),
                )
            })?;

        if args.query.trim().is_empty() {
            return Err(ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                "`query` must be a non-empty string",
            ));
        }

        let requested = args.num_results.unwrap_or(DEFAULT_NUM_RESULTS);
        let num_results = requested.clamp(1, MAX_NUM_RESULTS) as usize;

        let html = fetch_ddg_html(&self.client, &args.query).await?;
        let results = parse_ddg_results(&html, num_results);
        let formatted = format_results(&args.query, &results);

        Ok(ToolOutput::success(json!({
            "query": args.query,
            "results": results,
            "formatted": formatted,
        })))
    }
}

async fn fetch_ddg_html(client: &reqwest::Client, query: &str) -> Result<String, ToolError> {
    let response = client
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", query)])
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36",
        )
        .timeout(FALLBACK_TIMEOUT)
        .send()
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("DuckDuckGo request failed: {e}"),
        })?;

    let status = response.status();
    if !status.is_success() {
        return Err(ToolError::ExecutionFailed {
            reason: format!("DuckDuckGo returned non-success status: {status}"),
        });
    }

    response
        .text()
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("failed to read DuckDuckGo response body: {e}"),
        })
}

fn format_results(query: &str, results: &[SearchResult]) -> String {
    if results.is_empty() {
        return format!("No results found for: {query}");
    }
    let mut out = format!("Search results for: {query}\n\n");
    for (i, r) in results.iter().enumerate() {
        let _ = write!(
            out,
            "{idx}. **{title}**\n   {url}\n   {snippet}\n\n",
            idx = i + 1,
            title = r.title,
            url = r.url,
            snippet = r.snippet,
        );
    }
    out
}

mod regexes {
    use std::sync::OnceLock;

    use regex::Regex;

    fn compile(pattern: &str, label: &str) -> Option<Regex> {
        match Regex::new(pattern) {
            Ok(re) => Some(re),
            Err(err) => {
                tracing::warn!(label = label, error = %err, "web_search: regex compile failed");
                None
            }
        }
    }

    pub fn result_link() -> Option<&'static Regex> {
        static RE: OnceLock<Option<Regex>> = OnceLock::new();
        RE.get_or_init(|| {
            compile(
                r#"<a[^>]*class="result__a"[^>]*href="([^"]*)"[^>]*>([^<]*)</a>"#,
                "result_link",
            )
        })
        .as_ref()
    }

    pub fn result_snippet() -> Option<&'static Regex> {
        static RE: OnceLock<Option<Regex>> = OnceLock::new();
        RE.get_or_init(|| {
            // Lazy body match: the capture must stop at the snippet
            // element's own closing tag. A greedy inner-tag matcher
            // (`(?:<[^>]*>[^<]*)*`) swallows `</a>` itself and runs on to
            // the *last* closing tag in the document, folding every
            // subsequent result into the first snippet.
            compile(
                r#"(?s)<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#,
                "result_snippet",
            )
        })
        .as_ref()
    }

    pub fn tag() -> Option<&'static Regex> {
        static RE: OnceLock<Option<Regex>> = OnceLock::new();
        RE.get_or_init(|| compile(r"<[^>]+>", "tag")).as_ref()
    }
}

fn parse_ddg_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let (Some(link_re), Some(snippet_re), Some(tag_re)) = (
        regexes::result_link(),
        regexes::result_snippet(),
        regexes::tag(),
    ) else {
        return results;
    };

    let links: Vec<_> = link_re.captures_iter(html).collect();
    let snippets: Vec<_> = snippet_re.captures_iter(html).collect();

    for (i, link_cap) in links.iter().enumerate() {
        if results.len() >= max_results {
            break;
        }

        let raw_url = link_cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        let raw_title = link_cap.get(2).map(|m| m.as_str()).unwrap_or_default();
        let url = decode_ddg_url(raw_url);
        let title = html_decode(raw_title);

        if !url.starts_with("http") || url.contains("duckduckgo.com") {
            continue;
        }

        // Structural association: a snippet belongs to this link only if
        // it appears between this link and the next one in the document.
        // Parallel-array indexing would misattribute every snippet after
        // the first result that lacks one.
        let link_end = link_cap.get(0).map(|m| m.end()).unwrap_or_default();
        let next_link_start = links
            .get(i + 1)
            .and_then(|c| c.get(0))
            .map_or(html.len(), |m| m.start());
        let snippet = snippets
            .iter()
            .find(|c| {
                c.get(0).is_some_and(|whole| {
                    whole.start() >= link_end && whole.start() < next_link_start
                })
            })
            .and_then(|c| c.get(1))
            .map(|m| html_decode(&strip_tags(tag_re, m.as_str())))
            .unwrap_or_default();

        results.push(SearchResult {
            title,
            url,
            snippet,
        });
    }

    results
}

fn strip_tags(tag_re: &Regex, input: &str) -> String {
    tag_re.replace_all(input, "").into_owned()
}

fn decode_ddg_url(url: &str) -> String {
    // DDG wraps target URLs as //duckduckgo.com/l/?uddg=ENCODED&...
    let Some(uddg_start) = url.find("uddg=") else {
        return url.to_owned();
    };
    let start = uddg_start + "uddg=".len();
    let end = url[start..].find('&').map_or(url.len(), |i| start + i);
    let encoded = &url[start..end];
    percent_decode(encoded)
}

fn percent_decode(input: &str) -> String {
    // Minimal percent-decoder to avoid pulling in `urlencoding`. Handles
    // `%HH` triples; leaves invalid sequences intact so callers can still
    // see what came back from DDG.
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (
                char::from(bytes[i + 1]).to_digit(16),
                char::from(bytes[i + 2]).to_digit(16),
            )
            && let (Ok(h_u8), Ok(l_u8)) = (u8::try_from(h), u8::try_from(l))
        {
            out.push((h_u8 << 4) | l_u8);
            i += 3;
            continue;
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn html_decode(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_owned()
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

    use super::*;
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};

    fn envelope(args: Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_owned(),
            tool_name: "web_search".to_owned(),
            model_args: args,
            runtime_inputs: RuntimeInputs::default(),
            metadata: Value::Null,
        }
    }

    #[test]
    fn tool_object_safe() {
        let tool = WebSearchTool::new().expect("client builds");
        let _: Box<dyn Tool + Send + Sync> = Box::new(tool);
    }

    #[test]
    fn name_and_effect() {
        let tool = WebSearchTool::new().expect("client builds");
        assert_eq!(tool.name(), "web_search");
        assert_eq!(tool.effect(), ToolEffect::Network);
    }

    #[test]
    fn input_schema_declares_query_required() {
        let tool = WebSearchTool::new().expect("client builds");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().expect("required array");
        assert!(required.iter().any(|v| v == "query"));
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn parse_ddg_results_extracts_link_and_snippet() {
        let html = r#"
            <html><body>
            <div class="result">
              <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpost&amp;rut=abc">Example Post</a>
              <a class="result__snippet">A short <b>snippet</b> about the post.</a>
            </div>
            <div class="result">
              <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fpage">Example Org</a>
              <a class="result__snippet">Another snippet.</a>
            </div>
            </body></html>
        "#;
        let results = parse_ddg_results(html, 5);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Example Post");
        assert_eq!(results[0].url, "https://example.com/post");
        assert!(results[0].snippet.contains("snippet about the post"));
        assert_eq!(results[1].url, "https://example.org/page");
    }

    #[test]
    fn parse_ddg_results_filters_duckduckgo_internal_links() {
        let html = r#"
            <a class="result__a" href="https://duckduckgo.com/about">About DDG</a>
            <a class="result__snippet">should be skipped</a>
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F">Example</a>
            <a class="result__snippet">kept</a>
        "#;
        let results = parse_ddg_results(html, 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/");
    }

    /// Regression: snippets were zipped to links by parallel-array index,
    /// so one result without a snippet shifted every later snippet onto
    /// the wrong result. Association is structural (document position):
    /// the middle result here has no snippet and must stay empty while
    /// its neighbours keep their own text.
    #[test]
    fn missing_snippet_does_not_shift_attribution() {
        let html = r#"
            <div class="result">
              <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Ffirst.example%2F">First</a>
              <a class="result__snippet">first snippet</a>
            </div>
            <div class="result">
              <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fsecond.example%2F">Second (no snippet)</a>
            </div>
            <div class="result">
              <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fthird.example%2F">Third</a>
              <a class="result__snippet">third snippet</a>
            </div>
        "#;
        let results = parse_ddg_results(html, 5);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].url, "https://first.example/");
        assert_eq!(results[0].snippet, "first snippet");
        assert_eq!(results[1].url, "https://second.example/");
        assert_eq!(
            results[1].snippet, "",
            "a result without a snippet must not steal the next one",
        );
        assert_eq!(results[2].url, "https://third.example/");
        assert_eq!(results[2].snippet, "third snippet");
    }

    /// Filtered-out links (DDG-internal) must not consume the snippet
    /// window of the results around them.
    #[test]
    fn filtered_internal_link_does_not_misattribute_snippets() {
        let html = r#"
            <a class="result__a" href="https://duckduckgo.com/about">About DDG</a>
            <a class="result__snippet">internal snippet</a>
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fkept.example%2F">Kept</a>
            <a class="result__snippet">kept snippet</a>
        "#;
        let results = parse_ddg_results(html, 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://kept.example/");
        assert_eq!(results[0].snippet, "kept snippet");
    }

    #[test]
    fn parse_ddg_results_respects_max_results() {
        let mut html = String::new();
        for i in 0..10 {
            let _ = write!(
                html,
                r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fe{i}.com%2F">T{i}</a><a class="result__snippet">S{i}</a>"#,
            );
        }
        let results = parse_ddg_results(&html, 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn html_decode_handles_common_entities() {
        assert_eq!(html_decode("a &amp; b"), "a & b");
        assert_eq!(html_decode("&lt;tag&gt;"), "<tag>");
        assert_eq!(html_decode("don&#39;t"), "don't");
    }

    #[test]
    fn decode_ddg_url_unwraps_uddg() {
        let wrapped = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa%3Fb%3D1&rut=x";
        assert_eq!(decode_ddg_url(wrapped), "https://example.com/a?b=1");
    }

    #[tokio::test]
    async fn execute_rejects_empty_query() {
        let tool = WebSearchTool::new().expect("client builds");
        let env = envelope(json!({ "query": "" }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("empty query must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn execute_rejects_missing_query() {
        let tool = WebSearchTool::new().expect("client builds");
        let env = envelope(json!({}));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("missing query must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }
}
