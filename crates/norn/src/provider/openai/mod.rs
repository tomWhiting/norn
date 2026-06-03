//! Provider implementation for the `OpenAI` Responses API.

pub mod rate_limiter;
pub mod request;
pub mod sse;
mod sse_types;
pub mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;

use self::rate_limiter::RateLimiter;
use self::request::build_payload;
use self::sse::{SseParser, map_sse_event};
use crate::error::ProviderError;
use crate::provider::auth::{AuthProvider, AuthSource, build_from_auth_source};
use crate::provider::debug::DebugDumper;
use crate::provider::events::ProviderEvent;
use crate::provider::request::{ProviderConfig, ProviderRequest};
use crate::provider::traits::{Provider, ProviderStream};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const DEFAULT_PERMITS_PER_INTERVAL: u32 = 60;
const DEFAULT_RATE_LIMIT_INTERVAL: Duration = Duration::from_mins(1);
const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// `OpenAI` Responses API provider.
///
/// Shared across agents via `Arc`. Owns an HTTP client, a
/// token-bucket rate limiter, and the [`AuthProvider`] that
/// authenticates each outgoing request.
pub struct OpenAiProvider {
    client: reqwest::Client,
    config: ProviderConfig,
    rate_limiter: Arc<RateLimiter>,
    auth_provider: Arc<dyn AuthProvider>,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("base_url", &self.base_url())
            .field("timeout", &self.config.timeout)
            .field("max_retries", &self.config.max_retries)
            .finish_non_exhaustive()
    }
}

impl OpenAiProvider {
    /// Creates a new `OpenAI` provider from the given configuration.
    ///
    /// Builds the [`AuthProvider`] from `config.auth_source`. For
    /// `AuthSource::OAuth`, this initialises the underlying
    /// `codex-login` `AuthManager`, which may read from disk.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::ConnectionFailed`] if the HTTP client
    /// cannot be built, or [`ProviderError::AuthenticationFailed`] if
    /// the auth provider cannot be initialised.
    pub async fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        let client = build_http_client()?;

        let rate_limiter = Arc::new(RateLimiter::new(
            config.rate_limit.unwrap_or(DEFAULT_PERMITS_PER_INTERVAL),
            DEFAULT_RATE_LIMIT_INTERVAL,
        ));

        let auth_provider = build_from_auth_source(&config.auth_source).await?;

        Ok(Self {
            client,
            config,
            rate_limiter,
            auth_provider,
        })
    }

    /// Constructs a provider directly from a pre-built
    /// [`AuthProvider`]. Useful for tests using
    /// [`crate::provider::auth::MockAuthProvider`].
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::ConnectionFailed`] if the HTTP client
    /// cannot be built.
    pub fn with_auth_provider(
        config: ProviderConfig,
        auth_provider: Arc<dyn AuthProvider>,
    ) -> Result<Self, ProviderError> {
        let client = build_http_client()?;

        let rate_limiter = Arc::new(RateLimiter::new(
            config.rate_limit.unwrap_or(DEFAULT_PERMITS_PER_INTERVAL),
            DEFAULT_RATE_LIMIT_INTERVAL,
        ));

        Ok(Self {
            client,
            config,
            rate_limiter,
            auth_provider,
        })
    }

    fn base_url(&self) -> &str {
        match self.config.base_url.as_deref() {
            Some(url) => url,
            None if matches!(self.config.auth_source, AuthSource::OAuth { .. }) => CHATGPT_BASE_URL,
            None => DEFAULT_BASE_URL,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/responses", self.base_url())
    }
}

fn build_http_client() -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .pool_max_idle_per_host(4)
        .http2_keep_alive_interval(std::time::Duration::from_secs(30))
        .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .build()
        .map_err(|e| ProviderError::ConnectionFailed {
            reason: format!("failed to build HTTP client: {e}"),
        })
}

impl Provider for OpenAiProvider {
    fn capabilities(&self) -> crate::provider::tools::ProviderCapabilities {
        let is_chatgpt_backend = self.config.base_url.is_none()
            && matches!(self.config.auth_source, AuthSource::OAuth { .. });
        if is_chatgpt_backend {
            return crate::provider::tools::ProviderCapabilities {
                hosted_web_search: true,
                response_threading: false,
                server_compaction: false,
            };
        }
        crate::provider::tools::ProviderCapabilities::openai_responses()
    }

    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let client = self.client.clone();
        let endpoint = self.endpoint();
        let max_retries = self.config.max_retries;
        let rate_limiter = Arc::clone(&self.rate_limiter);
        let auth_provider = Arc::clone(&self.auth_provider);
        let debug_dump_file = self.config.debug_dump_file.clone();

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ProviderEvent, ProviderError>>(64);

        tokio::spawn(async move {
            let provider = SenderProvider {
                client,
                endpoint,
                max_retries,
                rate_limiter,
                auth_provider,
                debug_dump_file,
            };
            if let Err(e) = provider.execute(request, tx.clone()).await {
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

/// Internal helper that owns cloned state for the spawned task.
struct SenderProvider {
    client: reqwest::Client,
    endpoint: String,
    max_retries: u32,
    rate_limiter: Arc<RateLimiter>,
    auth_provider: Arc<dyn AuthProvider>,
    debug_dump_file: Option<PathBuf>,
}

impl SenderProvider {
    async fn execute(
        &self,
        request: ProviderRequest,
        tx: tokio::sync::mpsc::Sender<Result<ProviderEvent, ProviderError>>,
    ) -> Result<(), ProviderError> {
        let payload = build_payload(&request)?;
        let body =
            serde_json::to_string(&payload).map_err(|e| ProviderError::ResponseParseError {
                reason: format!("failed to serialize request: {e}"),
            })?;

        let dumper = self.debug_dump_file.as_deref().and_then(DebugDumper::new);
        if let Some(ref d) = dumper {
            d.write_request(&self.endpoint, &body);
        }

        let request_start = std::time::Instant::now();
        let msg_count = request.messages.len();
        tracing::debug!(
            endpoint = %self.endpoint,
            message_count = msg_count,
            "provider request starting"
        );

        let mut attempts = 0u32;
        let mut auth_retried = false;
        let response = loop {
            self.rate_limiter.acquire().await;

            let send_start = std::time::Instant::now();
            let mut builder = self
                .client
                .post(&self.endpoint)
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .body(body.clone());
            builder = self.auth_provider.apply_auth(builder).await?;

            let result = builder.send().await;

            let response = match result {
                Ok(resp) => {
                    let send_elapsed = send_start.elapsed();
                    tracing::debug!(
                        elapsed_s = send_elapsed.as_secs_f64(),
                        status = resp.status().as_u16(),
                        "provider response headers received"
                    );
                    resp
                }
                Err(e) => {
                    let send_elapsed = send_start.elapsed();
                    tracing::warn!(
                        elapsed_s = send_elapsed.as_secs_f64(),
                        is_timeout = e.is_timeout(),
                        is_connect = e.is_connect(),
                        error = %e,
                        "provider request failed"
                    );
                    if e.is_timeout() {
                        return Err(ProviderError::ConnectionFailed {
                            reason: format!("connection timed out: {e}"),
                        });
                    }
                    return Err(ProviderError::ConnectionFailed {
                        reason: format!("request failed: {e}"),
                    });
                }
            };

            let status = response.status();

            if status == reqwest::StatusCode::UNAUTHORIZED {
                if auth_retried {
                    return Err(ProviderError::AuthenticationFailed {
                        reason: "HTTP 401 Unauthorized after token refresh".to_string(),
                    });
                }
                auth_retried = true;
                match self.auth_provider.on_unauthorized().await {
                    Ok(true) => continue,
                    Ok(false) => {
                        return Err(ProviderError::AuthenticationFailed {
                            reason: "HTTP 401 Unauthorized and no refresh available".to_string(),
                        });
                    }
                    Err(e) => return Err(e),
                }
            }

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                attempts += 1;
                if attempts > self.max_retries {
                    return Err(ProviderError::RateLimited { retry_after: None });
                }

                let retry_after = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map_or(DEFAULT_RETRY_BACKOFF, Duration::from_secs);

                self.rate_limiter.adjust_interval(retry_after).await;
                tokio::time::sleep(retry_after).await;
                continue;
            }

            if status.is_server_error() {
                let body_text = response.text().await.unwrap_or_default();
                return Err(ProviderError::StreamError {
                    reason: format!("HTTP {status}: {body_text}"),
                });
            }

            if !status.is_success() {
                let body_text = response.text().await.unwrap_or_default();
                return Err(ProviderError::StreamError {
                    reason: format!("HTTP {status}: {body_text}"),
                });
            }

            break response;
        };

        if let Some(ref d) = dumper {
            let status = response.status().as_u16();
            let headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_owned()))
                .collect();
            d.write_response_meta(status, &headers);
        }

        let stream_start = std::time::Instant::now();
        let mut chunk_count: u64 = 0;
        let mut event_count: u64 = 0;
        let mut last_chunk = std::time::Instant::now();

        let mut parser = SseParser::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| {
                let stream_elapsed = stream_start.elapsed();
                let since_last = last_chunk.elapsed();
                tracing::warn!(
                    stream_elapsed_s = stream_elapsed.as_secs_f64(),
                    since_last_chunk_s = since_last.as_secs_f64(),
                    chunks_received = chunk_count,
                    events_parsed = event_count,
                    error = %e,
                    "SSE stream interrupted"
                );
                ProviderError::StreamInterrupted {
                    reason: format!("connection lost mid-stream: {e}"),
                }
            })?;
            chunk_count += 1;
            last_chunk = std::time::Instant::now();
            for sse_event in parser.feed(chunk.as_ref()) {
                event_count += 1;
                if let Some(ref d) = dumper {
                    d.write_sse_event(&sse_event.event_type, &sse_event.data);
                }
                if let Some(provider_event) = map_sse_event(&sse_event) {
                    let is_done = matches!(provider_event, Ok(ProviderEvent::Done { .. }));
                    if tx.send(provider_event).await.is_err() {
                        return Ok(());
                    }
                    if is_done {
                        let total_elapsed = request_start.elapsed();
                        tracing::debug!(
                            total_s = total_elapsed.as_secs_f64(),
                            stream_s = stream_start.elapsed().as_secs_f64(),
                            chunks = chunk_count,
                            events = event_count,
                            "provider request complete"
                        );
                        return Ok(());
                    }
                }
            }
        }

        for sse_event in parser.finish() {
            event_count += 1;
            if let Some(ref d) = dumper {
                d.write_sse_event(&sse_event.event_type, &sse_event.data);
            }
            if let Some(provider_event) = map_sse_event(&sse_event)
                && tx.send(provider_event).await.is_err()
            {
                return Ok(());
            }
        }

        let total_elapsed = request_start.elapsed();
        tracing::debug!(
            total_s = total_elapsed.as_secs_f64(),
            stream_s = stream_start.elapsed().as_secs_f64(),
            chunks = chunk_count,
            events = event_count,
            "provider request complete"
        );

        Ok(())
    }
}

const _: fn() = || {
    fn check<T: Send + Sync>() {}
    check::<OpenAiProvider>();
    check::<Arc<OpenAiProvider>>();
};

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
    use crate::provider::auth::{AuthSource, MockAuthProvider};
    use crate::provider::request::SecretString;

    fn test_config() -> ProviderConfig {
        ProviderConfig {
            auth_source: AuthSource::ApiKey {
                key: SecretString::new("test-key"),
            },
            base_url: Some("http://localhost:9999/v1".to_string()),
            timeout: Duration::from_secs(5),
            max_retries: 2,
            provider_options: None,
            debug_dump_file: None,
            rate_limit: None,
        }
    }

    fn test_provider() -> OpenAiProvider {
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
        OpenAiProvider::with_auth_provider(test_config(), mock).expect("create")
    }

    #[test]
    fn debug_does_not_expose_api_key() {
        let provider = test_provider();
        let debug = format!("{provider:?}");
        assert!(!debug.contains("test-key"));
        assert!(debug.contains("OpenAiProvider"));
    }

    #[test]
    fn arc_openai_provider_compiles() {
        let provider = test_provider();
        let _arc: Arc<OpenAiProvider> = Arc::new(provider);
    }

    #[test]
    fn default_base_url() {
        let mut config = test_config();
        config.base_url = None;
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("k"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");
        assert_eq!(provider.base_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn custom_base_url() {
        let provider = test_provider();
        assert_eq!(provider.base_url(), "http://localhost:9999/v1");
    }

    #[test]
    fn chatgpt_oauth_capabilities_do_not_enable_response_threading() {
        let mut config = test_config();
        config.auth_source = AuthSource::OAuth { codex_home: None };
        config.base_url = None;
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("oauth-token"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");

        let capabilities = provider.capabilities();

        assert!(capabilities.hosted_web_search);
        assert!(!capabilities.response_threading);
        assert!(!capabilities.server_compaction);
    }

    #[test]
    fn api_key_openai_capabilities_keep_responses_state_features() {
        let mut config = test_config();
        config.base_url = None;
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("api-key"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");

        let capabilities = provider.capabilities();

        assert!(capabilities.hosted_web_search);
        assert!(capabilities.response_threading);
        assert!(capabilities.server_compaction);
    }

    #[test]
    fn endpoint_construction() {
        let provider = test_provider();
        assert_eq!(provider.endpoint(), "http://localhost:9999/v1/responses");
    }

    #[test]
    fn rate_limit_none_uses_default_permits() {
        let mut config = test_config();
        config.rate_limit = None;
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("k"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");
        assert_eq!(
            provider.rate_limiter.permits_per_interval(),
            DEFAULT_PERMITS_PER_INTERVAL,
        );
    }

    #[test]
    fn rate_limit_some_overrides_default_permits() {
        let mut config = test_config();
        config.rate_limit = Some(120);
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("k"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");
        assert_eq!(provider.rate_limiter.permits_per_interval(), 120);
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
mod integration_tests {
    use super::*;
    use crate::provider::auth::AuthSource;
    use crate::provider::events::ProviderEvent;
    use crate::provider::request::{
        Message, MessageRole, ProviderConfig, ProviderRequest, SecretString,
    };
    use futures_util::StreamExt;

    #[tokio::test]
    async fn openai_integration_test() {
        let api_key = match std::env::var("OPENAI_TEST_KEY") {
            Ok(key) if !key.is_empty() => key,
            _ => {
                tracing::info!("OPENAI_TEST_KEY not set, skipping");
                return;
            }
        };

        let config = ProviderConfig {
            auth_source: AuthSource::ApiKey {
                key: SecretString::new(api_key),
            },
            base_url: None,
            timeout: Duration::from_secs(30),
            max_retries: 2,
            provider_options: None,
            debug_dump_file: None,
            rate_limit: None,
        };

        let provider = OpenAiProvider::new(config).await.expect("create provider");
        let request = ProviderRequest {
            messages: vec![Message {
                role: MessageRole::User,
                content: Some("Say hello in exactly one word.".to_string()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            }],
            tools: vec![],
            model: "gpt-4.1-mini".to_string(),
            reasoning_effort: None,
            reasoning_summary: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        };

        let mut stream = provider.stream(request).expect("stream");
        let mut saw_text_delta = false;
        let mut saw_done = false;

        while let Some(event) = stream.next().await {
            match event {
                Ok(ProviderEvent::TextDelta { .. }) => saw_text_delta = true,
                Ok(ProviderEvent::Done { .. }) => saw_done = true,
                Ok(_) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert!(saw_text_delta, "expected at least one TextDelta event");
        assert!(saw_done, "expected a Done event");
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
    clippy::match_wildcard_for_single_variants,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::struct_field_names,
    clippy::large_stack_arrays,
    clippy::single_match_else,
    clippy::needless_continue
)]
mod streaming_tests {
    use super::*;
    use crate::provider::auth::{AuthSource, MockAuthProvider};
    use crate::provider::events::ProviderEvent;
    use crate::provider::request::{
        Message, MessageRole, ProviderConfig, ProviderRequest, SecretString,
    };
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn build_request() -> ProviderRequest {
        ProviderRequest {
            messages: vec![Message {
                role: MessageRole::User,
                content: Some("hello".to_string()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            }],
            tools: vec![],
            model: "gpt-test".to_string(),
            reasoning_effort: None,
            reasoning_summary: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        }
    }

    fn build_config(base_url: String, max_retries: u32) -> ProviderConfig {
        ProviderConfig {
            auth_source: AuthSource::ApiKey {
                key: SecretString::new("test-key"),
            },
            base_url: Some(base_url),
            timeout: Duration::from_secs(10),
            max_retries,
            provider_options: None,
            debug_dump_file: None,
            rate_limit: None,
        }
    }

    fn build_provider(base_url: String, max_retries: u32) -> OpenAiProvider {
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
        OpenAiProvider::with_auth_provider(
            build_config(format!("{base_url}/v1"), max_retries),
            mock,
        )
        .expect("create")
    }

    fn sse_completed_frame() -> &'static str {
        "event: response.completed\n\
         data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\n"
    }

    async fn drain_request(socket: &mut tokio::net::TcpStream) {
        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 4096];
        let mut headers_end: Option<usize> = None;
        let mut content_length: Option<usize> = None;
        loop {
            let n = socket.read(&mut tmp).await.unwrap();
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&tmp[..n]);
            if headers_end.is_none()
                && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
            {
                headers_end = Some(pos + 4);
                let headers_text = String::from_utf8_lossy(&buf[..pos]);
                for line in headers_text.lines() {
                    let lc = line.to_ascii_lowercase();
                    if let Some(value) = lc.strip_prefix("content-length:") {
                        content_length = value.trim().parse::<usize>().ok();
                        break;
                    }
                }
            }
            match (headers_end, content_length) {
                (Some(end), Some(cl)) if buf.len() >= end + cl => return,
                (Some(_), None) => return,
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn multi_frame_sse_delivered_end_to_end() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = "event: response.output_text.delta\n\
                    data: {\"delta\":\"hello\"}\n\n\
                    event: response.output_text.delta\n\
                    data: {\"delta\":\"world\"}\n\n\
                    event: response.completed\n\
                    data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\n";

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let provider = build_provider(server.uri(), 0);
        let mut stream = provider.stream(build_request()).expect("stream");

        let mut deltas = Vec::new();
        let mut got_done = false;
        while let Some(evt) = stream.next().await {
            match evt {
                Ok(ProviderEvent::TextDelta { text }) => deltas.push(text),
                Ok(ProviderEvent::Done { .. }) => got_done = true,
                Ok(_) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        assert_eq!(deltas, vec!["hello".to_string(), "world".to_string()]);
        assert!(got_done, "expected Done event");
    }

    #[tokio::test]
    async fn streamed_events_arrive_incrementally() {
        // Custom TCP listener with paced chunked-encoding writes — verifies
        // the consumer receives each event before the server emits the next.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let frame1 = "event: response.output_text.delta\ndata: {\"delta\":\"a\"}\n\n";
        let frame2 = "event: response.output_text.delta\ndata: {\"delta\":\"b\"}\n\n";
        let frame3 = sse_completed_frame();
        let gap = Duration::from_millis(80);

        let server_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            drain_request(&mut socket).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      Content-Type: text/event-stream\r\n\
                      Transfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .unwrap();
            socket.flush().await.unwrap();
            for (i, frame) in [frame1, frame2, frame3].iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(gap).await;
                }
                let chunk = format!("{:X}\r\n{}\r\n", frame.len(), frame);
                socket.write_all(chunk.as_bytes()).await.unwrap();
                socket.flush().await.unwrap();
            }
            socket.write_all(b"0\r\n\r\n").await.unwrap();
            socket.flush().await.unwrap();
        });

        let provider = build_provider(format!("http://127.0.0.1:{port}"), 0);
        let mut stream = provider.stream(build_request()).expect("stream");

        let start = std::time::Instant::now();
        let mut arrivals: Vec<(Duration, Result<ProviderEvent, ProviderError>)> = Vec::new();
        while let Some(evt) = stream.next().await {
            arrivals.push((start.elapsed(), evt));
        }
        server_task.await.unwrap();

        let text_delta_count = arrivals
            .iter()
            .filter(|(_, e)| matches!(e, Ok(ProviderEvent::TextDelta { .. })))
            .count();
        let done_count = arrivals
            .iter()
            .filter(|(_, e)| matches!(e, Ok(ProviderEvent::Done { .. })))
            .count();
        assert_eq!(text_delta_count, 2, "expected 2 TextDelta events");
        assert_eq!(done_count, 1, "expected 1 Done event");

        // Incremental delivery: total span between first and last event must
        // exceed at least one inter-chunk gap. If the response were buffered,
        // all events would arrive within a few milliseconds of each other.
        let first = arrivals.first().unwrap().0;
        let last = arrivals.last().unwrap().0;
        assert!(
            last >= first + Duration::from_millis(60),
            "events did not arrive incrementally: first={first:?}, last={last:?}"
        );
    }

    #[tokio::test]
    async fn retry_after_429_streams_successfully() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = format!(
            "event: response.output_text.delta\n\
             data: {{\"delta\":\"after-retry\"}}\n\n{}",
            sse_completed_frame()
        );

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .with_priority(2)
            .mount(&server)
            .await;

        let provider = build_provider(server.uri(), 3);

        let start = std::time::Instant::now();
        let mut stream = provider.stream(build_request()).expect("stream");

        let mut delta_text = String::new();
        let mut got_done = false;
        while let Some(evt) = stream.next().await {
            match evt {
                Ok(ProviderEvent::TextDelta { text }) => delta_text.push_str(&text),
                Ok(ProviderEvent::Done { .. }) => got_done = true,
                Ok(_) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        let elapsed = start.elapsed();

        assert_eq!(delta_text, "after-retry");
        assert!(got_done, "expected Done event after retry");
        assert!(
            elapsed >= Duration::from_millis(900),
            "Retry-After: 1 should have been respected (elapsed: {elapsed:?})"
        );
    }

    #[tokio::test]
    async fn mid_stream_connection_drop_yields_stream_interrupted() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let frame = "event: response.output_text.delta\ndata: {\"delta\":\"partial\"}\n\n";

        let server_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            drain_request(&mut socket).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      Content-Type: text/event-stream\r\n\
                      Transfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .unwrap();
            let chunk = format!("{:X}\r\n{}\r\n", frame.len(), frame);
            socket.write_all(chunk.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            // Give the client time to receive and parse the chunk.
            tokio::time::sleep(Duration::from_millis(50)).await;
            // Drop without terminating chunk — chunked encoding sees premature end.
            drop(socket);
        });

        let provider = build_provider(format!("http://127.0.0.1:{port}"), 0);
        let mut stream = provider.stream(build_request()).expect("stream");

        let mut got_delta = false;
        let mut got_interrupted = false;
        while let Some(evt) = stream.next().await {
            match evt {
                Ok(ProviderEvent::TextDelta { text }) => {
                    assert_eq!(text, "partial");
                    got_delta = true;
                }
                Err(ProviderError::StreamInterrupted { reason }) => {
                    assert!(reason.contains("mid-stream"));
                    got_interrupted = true;
                    break;
                }
                Ok(_) => {}
                Err(e) => panic!("unexpected error variant: {e}"),
            }
        }
        server_task.await.unwrap();
        assert!(got_delta, "expected TextDelta before stream interruption");
        assert!(
            got_interrupted,
            "expected ProviderError::StreamInterrupted after socket drop"
        );
    }

    #[tokio::test]
    async fn recovers_from_401_via_auth_refresh() {
        use wiremock::matchers::{header, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = format!(
            "event: response.output_text.delta\n\
             data: {{\"delta\":\"after-refresh\"}}\n\n{}",
            sse_completed_frame()
        );

        Mock::given(method("POST"))
            .and(header("Authorization", "Bearer stale-token"))
            .respond_with(ResponseTemplate::new(401))
            .with_priority(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(header("Authorization", "Bearer fresh-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .with_priority(2)
            .mount(&server)
            .await;

        let mock_auth = MockAuthProvider::with_token_sequence(vec![
            "stale-token".to_string(),
            "fresh-token".to_string(),
        ])
        .with_unauthorized_responses(vec![Ok(true)]);
        let mock_auth_arc = Arc::new(mock_auth);
        let auth_for_provider: Arc<dyn AuthProvider> = mock_auth_arc.clone();

        let provider =
            OpenAiProvider::with_auth_provider(build_config(server.uri(), 0), auth_for_provider)
                .expect("create");

        let mut stream = provider.stream(build_request()).expect("stream");
        let mut delta_text = String::new();
        let mut got_done = false;
        while let Some(evt) = stream.next().await {
            match evt {
                Ok(ProviderEvent::TextDelta { text }) => delta_text.push_str(&text),
                Ok(ProviderEvent::Done { .. }) => got_done = true,
                Ok(_) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert_eq!(delta_text, "after-refresh");
        assert!(got_done, "expected Done event after refresh");
        assert_eq!(
            mock_auth_arc.refresh_call_count(),
            1,
            "expected exactly one refresh attempt"
        );
        assert_eq!(
            mock_auth_arc.apply_call_count(),
            2,
            "expected two apply_auth calls: stale + fresh"
        );
    }

    #[tokio::test]
    async fn fails_after_401_when_refresh_returns_false() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let mock_auth =
            MockAuthProvider::single("any-token").with_unauthorized_responses(vec![Ok(false)]);
        let mock_auth_arc = Arc::new(mock_auth);
        let auth_for_provider: Arc<dyn AuthProvider> = mock_auth_arc.clone();

        let provider =
            OpenAiProvider::with_auth_provider(build_config(server.uri(), 0), auth_for_provider)
                .expect("create");

        let mut stream = provider.stream(build_request()).expect("stream");
        let mut got_auth_error = false;
        while let Some(evt) = stream.next().await {
            match evt {
                Err(ProviderError::AuthenticationFailed { reason }) => {
                    assert!(reason.contains("401"));
                    got_auth_error = true;
                    break;
                }
                Ok(_) => {}
                Err(e) => panic!("unexpected error variant: {e}"),
            }
        }
        assert!(got_auth_error, "expected AuthenticationFailed");
        assert_eq!(
            mock_auth_arc.refresh_call_count(),
            1,
            "should have attempted refresh exactly once"
        );
    }
}
