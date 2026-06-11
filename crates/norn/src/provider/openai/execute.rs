//! Request execution: retry loop, status handling, and SSE consumption.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;

use super::rate_limiter::RateLimiter;
use super::request::build_payload;
use super::retry_after::parse_retry_after;
use super::sse::{SseParser, map_sse_event};
use crate::error::ProviderError;
use crate::provider::auth::AuthProvider;
use crate::provider::debug::DebugDumper;
use crate::provider::events::ProviderEvent;
use crate::provider::request::ProviderRequest;

/// Deliberate, owner-approved default (2026-06-11) used when
/// [`ProviderConfig::retry_backoff`] is `None`: the wait applied to a
/// `429` response that carries no parseable `Retry-After` header.
///
/// [`ProviderConfig::retry_backoff`]: crate::provider::request::ProviderConfig::retry_backoff
pub(super) const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// Internal helper that owns cloned provider state for the spawned
/// per-request task.
pub(super) struct SenderProvider {
    pub(super) client: reqwest::Client,
    pub(super) endpoint: String,
    /// Stall deadline from [`ProviderConfig::timeout`]: bounds the wait
    /// for response headers and the gap between SSE chunks. Not a
    /// whole-request deadline — streams are legitimately long-lived.
    ///
    /// [`ProviderConfig::timeout`]: crate::provider::request::ProviderConfig::timeout
    pub(super) timeout: Duration,
    pub(super) max_retries: u32,
    /// Wait applied to a `429` without a parseable `Retry-After`
    /// header. Resolved from [`ProviderConfig::retry_backoff`], falling
    /// back to [`DEFAULT_RETRY_BACKOFF`].
    ///
    /// [`ProviderConfig::retry_backoff`]: crate::provider::request::ProviderConfig::retry_backoff
    pub(super) retry_backoff: Duration,
    /// Optional ceiling on accepted server-supplied `Retry-After`
    /// waits, from [`ProviderConfig::retry_after_ceiling`]. `None`
    /// honors the header as-is.
    ///
    /// [`ProviderConfig::retry_after_ceiling`]: crate::provider::request::ProviderConfig::retry_after_ceiling
    pub(super) retry_after_ceiling: Option<Duration>,
    pub(super) rate_limiter: Arc<RateLimiter>,
    pub(super) auth_provider: Arc<dyn AuthProvider>,
    pub(super) debug_dump_file: Option<PathBuf>,
}

impl SenderProvider {
    pub(super) async fn execute(
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

            let result = match tokio::time::timeout(self.timeout, builder.send()).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    tracing::warn!(
                        elapsed_s = send_start.elapsed().as_secs_f64(),
                        timeout_s = self.timeout.as_secs_f64(),
                        "provider request timed out waiting for response headers"
                    );
                    return Err(ProviderError::ConnectionFailed {
                        reason: format!(
                            "connection timed out: no response headers within {:.1}s",
                            self.timeout.as_secs_f64()
                        ),
                    });
                }
            };

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
                let parsed_retry_after = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_retry_after);
                // Clamp the server-supplied wait to the configured
                // ceiling, when one is set. The clamped value is the
                // *accepted* value: it is what gets slept on, imposed
                // on the shared limiter, and surfaced in `RateLimited`,
                // so a hostile header can never push an absurd wait
                // anywhere. With no ceiling the header is honored
                // as-is; everything downstream uses saturating
                // arithmetic, so such a value can stall requests
                // against this provider but can never panic.
                let retry_after = match (parsed_retry_after, self.retry_after_ceiling) {
                    (Some(wait), Some(ceiling)) => Some(wait.min(ceiling)),
                    (parsed, _) => parsed,
                };
                let wait = retry_after.unwrap_or(self.retry_backoff);

                // Impose back-pressure on every caller sharing this
                // limiter for the server-requested window; the gate
                // expires on its own so throughput decays back to the
                // configured baseline. Applied even when this request
                // is out of retries: the server's signal still governs
                // every other in-flight caller.
                self.rate_limiter.impose_cooldown(wait).await;

                attempts += 1;
                if attempts > self.max_retries {
                    return Err(ProviderError::RateLimited { retry_after });
                }

                tokio::time::sleep(wait).await;
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
        loop {
            let Ok(next) = tokio::time::timeout(self.timeout, stream.next()).await else {
                tracing::warn!(
                    stream_elapsed_s = stream_start.elapsed().as_secs_f64(),
                    since_last_chunk_s = last_chunk.elapsed().as_secs_f64(),
                    chunks_received = chunk_count,
                    events_parsed = event_count,
                    timeout_s = self.timeout.as_secs_f64(),
                    "SSE stream inactivity deadline expired"
                );
                return Err(ProviderError::StreamError {
                    reason: format!(
                        "SSE stream timed out: no data received for {:.1}s",
                        self.timeout.as_secs_f64()
                    ),
                });
            };
            let Some(chunk_result) = next else {
                break;
            };
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
    use crate::r#loop::retry::RetryPolicy;
    use crate::provider::auth::{AuthProvider, AuthSource, MockAuthProvider};
    use crate::provider::events::ProviderEvent;
    use crate::provider::openai::OpenAiProvider;
    use crate::provider::request::{
        Message, MessageRole, ProviderConfig, ProviderRequest, SecretString,
    };
    use crate::provider::traits::Provider as _;
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

    fn build_config(base_url: String, max_retries: u32, timeout: Duration) -> ProviderConfig {
        ProviderConfig {
            auth_source: AuthSource::ApiKey {
                key: SecretString::new("test-key"),
            },
            base_url: Some(base_url),
            timeout,
            max_retries,
            provider_options: None,
            debug_dump_file: None,
            rate_limit: None,
            rate_limit_interval: None,
            retry_backoff: None,
            retry_after_ceiling: None,
        }
    }

    fn build_provider(base_url: String, max_retries: u32) -> OpenAiProvider {
        build_provider_with_timeout(base_url, max_retries, Duration::from_secs(10))
    }

    fn build_provider_with_timeout(
        base_url: String,
        max_retries: u32,
        timeout: Duration,
    ) -> OpenAiProvider {
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
        OpenAiProvider::with_auth_provider(
            build_config(format!("{base_url}/v1"), max_retries, timeout),
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

    /// When retries are exhausted on a 429 that carried `Retry-After`,
    /// the terminal [`ProviderError::RateLimited`] must surface the
    /// parsed value instead of discarding it as `None` — callers one
    /// layer up use it to schedule their own retry.
    #[tokio::test]
    async fn exhausted_429_retries_surface_server_retry_after() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "7"))
            .mount(&server)
            .await;

        let provider = build_provider(server.uri(), 0);
        let mut stream = provider.stream(build_request()).expect("stream");

        let evt = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("stream must fail fast when out of retries")
            .expect("stream must yield a terminal event");
        match evt {
            Err(ProviderError::RateLimited { retry_after }) => {
                assert_eq!(
                    retry_after,
                    Some(Duration::from_secs(7)),
                    "server-provided Retry-After must be surfaced"
                );
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    /// Regression test for the unbounded server-controlled `Retry-After`
    /// (fix campaign Track V, finding 1): a single 429 response carrying
    /// `Retry-After: 18446744073709551615` previously panicked the
    /// spawned provider task inside `RateLimiter::impose_cooldown`
    /// (`Instant + Duration` overflow), so the consumer stream ended
    /// with neither `Done` nor an error. The stream must instead yield
    /// a terminal [`ProviderError::RateLimited`] carrying the parsed
    /// value.
    #[tokio::test]
    async fn u64_max_retry_after_does_not_panic_and_surfaces_rate_limited() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429).insert_header("Retry-After", "18446744073709551615"),
            )
            .mount(&server)
            .await;

        let provider = build_provider(server.uri(), 0);
        let mut stream = provider.stream(build_request()).expect("stream");

        let evt = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("stream must terminate promptly when out of retries")
            .expect("stream must yield a terminal event, not end silently");
        match evt {
            Err(ProviderError::RateLimited { retry_after }) => {
                assert_eq!(
                    retry_after,
                    Some(Duration::from_secs(u64::MAX)),
                    "with no ceiling configured the header is honored as-is"
                );
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    /// With [`ProviderConfig::retry_after_ceiling`] set, an absurd
    /// delta-seconds `Retry-After` is clamped: the request retries
    /// after the ceiling instead of sleeping for the server-requested
    /// hour, and succeeds promptly.
    #[tokio::test]
    async fn retry_after_ceiling_clamps_delta_seconds() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = format!(
            "event: response.output_text.delta\n\
             data: {{\"delta\":\"after-clamp\"}}\n\n{}",
            sse_completed_frame()
        );

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "3600"))
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

        let mut config = build_config(format!("{}/v1", server.uri()), 3, Duration::from_secs(10));
        config.retry_after_ceiling = Some(Duration::from_millis(200));
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");

        let start = std::time::Instant::now();
        let mut stream = provider.stream(build_request()).expect("stream");

        let mut delta_text = String::new();
        let mut got_done = false;
        let collected = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(evt) = stream.next().await {
                match evt {
                    Ok(ProviderEvent::TextDelta { text }) => delta_text.push_str(&text),
                    Ok(ProviderEvent::Done { .. }) => got_done = true,
                    Ok(_) => {}
                    Err(e) => panic!("unexpected error: {e}"),
                }
            }
        })
        .await;
        let elapsed = start.elapsed();

        assert!(
            collected.is_ok(),
            "ceiling must bound the wait; stream hung past 5s"
        );
        assert_eq!(delta_text, "after-clamp");
        assert!(got_done, "expected Done event after clamped retry");
        assert!(
            elapsed >= Duration::from_millis(180),
            "the clamped wait must still be respected (elapsed: {elapsed:?})"
        );
    }

    /// With a ceiling set, the *surfaced* `RateLimited::retry_after` is
    /// the clamped (accepted) value, so a hostile header cannot push an
    /// absurd wait into callers that schedule their own retry on it.
    #[tokio::test]
    async fn exhausted_429_retries_surface_clamped_retry_after_when_ceiling_set() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429).insert_header("Retry-After", "18446744073709551615"),
            )
            .mount(&server)
            .await;

        let mut config = build_config(format!("{}/v1", server.uri()), 0, Duration::from_secs(10));
        config.retry_after_ceiling = Some(Duration::from_millis(250));
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");
        let mut stream = provider.stream(build_request()).expect("stream");

        let evt = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("stream must fail fast when out of retries")
            .expect("stream must yield a terminal event");
        match evt {
            Err(ProviderError::RateLimited { retry_after }) => {
                assert_eq!(
                    retry_after,
                    Some(Duration::from_millis(250)),
                    "surfaced retry_after must be the clamped (accepted) value"
                );
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    /// A far-future HTTP-date `Retry-After` (year 9999) parses to a
    /// millennia-scale wait; with a ceiling configured the retry happens
    /// after the ceiling instead of stalling the task for centuries.
    #[tokio::test]
    async fn far_future_http_date_retry_after_is_clamped_to_ceiling() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = format!(
            "event: response.output_text.delta\n\
             data: {{\"delta\":\"after-far-future\"}}\n\n{}",
            sse_completed_frame()
        );

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "Fri, 31 Dec 9999 23:59:59 +0000"),
            )
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

        let mut config = build_config(format!("{}/v1", server.uri()), 3, Duration::from_secs(10));
        config.retry_after_ceiling = Some(Duration::from_millis(200));
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");
        let mut stream = provider.stream(build_request()).expect("stream");

        let mut delta_text = String::new();
        let mut got_done = false;
        let collected = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(evt) = stream.next().await {
                match evt {
                    Ok(ProviderEvent::TextDelta { text }) => delta_text.push_str(&text),
                    Ok(ProviderEvent::Done { .. }) => got_done = true,
                    Ok(_) => {}
                    Err(e) => panic!("unexpected error: {e}"),
                }
            }
        })
        .await;

        assert!(
            collected.is_ok(),
            "far-future HTTP-date must be clamped by the ceiling; stream hung past 5s"
        );
        assert_eq!(delta_text, "after-far-future");
        assert!(got_done, "expected Done event after clamped retry");
    }

    /// `ProviderConfig::retry_backoff` replaces the owner-approved 1s
    /// default for header-less 429 responses: with a 50ms backoff the
    /// retry completes well inside the default's 1s wait.
    #[tokio::test]
    async fn configured_retry_backoff_overrides_default() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = format!(
            "event: response.output_text.delta\n\
             data: {{\"delta\":\"after-fast-backoff\"}}\n\n{}",
            sse_completed_frame()
        );

        // Header-less 429: the configured backoff governs the wait.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429))
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

        let mut config = build_config(format!("{}/v1", server.uri()), 3, Duration::from_secs(10));
        config.retry_backoff = Some(Duration::from_millis(50));
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");

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

        assert_eq!(delta_text, "after-fast-backoff");
        assert!(got_done, "expected Done event after retry");
        assert!(
            elapsed >= Duration::from_millis(40),
            "configured backoff must be respected (elapsed: {elapsed:?})"
        );
        assert!(
            elapsed < Duration::from_millis(900),
            "configured 50ms backoff must replace the 1s default (elapsed: {elapsed:?})"
        );
    }

    /// Regression test for HTTP-date `Retry-After` values (REVIEW.md H5):
    /// previously only delta-seconds parsed; an HTTP-date fell back to the
    /// 1s default instead of the server-requested deadline.
    #[tokio::test]
    async fn retry_after_http_date_is_honored() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = format!(
            "event: response.output_text.delta\n\
             data: {{\"delta\":\"after-date-retry\"}}\n\n{}",
            sse_completed_frame()
        );

        // `start` is captured *before* the date is generated so the
        // assertion is anchored to the server-requested deadline rather
        // than to however long mock setup takes under test-suite load.
        // `to_rfc2822` truncates sub-seconds, so the deadline is at
        // least `start + 2s` and the client must not finish before it.
        let start = std::time::Instant::now();
        let retry_at = (chrono::Utc::now() + chrono::Duration::seconds(3)).to_rfc2822();
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", retry_at))
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

        assert_eq!(delta_text, "after-date-retry");
        assert!(got_done, "expected Done event after retry");
        // The deadline is at least `start + 2s` (3s offset minus at most
        // 1s of rfc2822 second-truncation) and the client sleeps until
        // it, so a correct implementation can never finish earlier. The
        // pre-fix fallback slept a flat 1s instead and fails this bound.
        assert!(
            elapsed >= Duration::from_secs(2),
            "HTTP-date Retry-After should have been respected (elapsed: {elapsed:?})"
        );
    }

    /// Regression test for REVIEW.md H4: a server that accepts the
    /// request but never sends response headers must trip the configured
    /// timeout (as a retryable network timeout) instead of hanging the
    /// turn indefinitely.
    #[tokio::test]
    async fn unresponsive_server_times_out_before_response_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            drain_request(&mut socket).await;
            // Never respond; hold the socket open well past the deadline.
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(socket);
        });

        let provider = build_provider_with_timeout(
            format!("http://127.0.0.1:{port}"),
            0,
            Duration::from_millis(300),
        );
        let mut stream = provider.stream(build_request()).expect("stream");

        let outcome = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
        server_task.abort();

        let Ok(Some(Err(err))) = outcome else {
            panic!("expected a timeout error within 5s, got {outcome:?}");
        };
        match &err {
            ProviderError::ConnectionFailed { reason } => {
                assert!(
                    reason.contains("timed out"),
                    "reason should mention the timeout: {reason}"
                );
            }
            other => panic!("expected ConnectionFailed, got {other:?}"),
        }
        assert!(
            RetryPolicy::default().classifies_as_retryable(&err),
            "header-wait timeout must classify as retryable"
        );
    }

    /// Regression test for REVIEW.md H4: a stream that goes silent
    /// mid-response must trip the configured inactivity deadline (as a
    /// retryable network timeout) instead of hanging the turn.
    #[tokio::test]
    async fn stalled_sse_stream_times_out_as_retryable_network_timeout() {
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
            // Stall without closing: keep the connection open, send nothing.
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(socket);
        });

        let provider = build_provider_with_timeout(
            format!("http://127.0.0.1:{port}"),
            0,
            Duration::from_millis(300),
        );
        let mut stream = provider.stream(build_request()).expect("stream");

        let collected = tokio::time::timeout(Duration::from_secs(5), async {
            let mut events = Vec::new();
            while let Some(evt) = stream.next().await {
                let is_err = evt.is_err();
                events.push(evt);
                if is_err {
                    break;
                }
            }
            events
        })
        .await;
        server_task.abort();

        let Ok(events) = collected else {
            panic!("stream hung past the 5s harness deadline: inactivity timeout not applied");
        };
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Ok(ProviderEvent::TextDelta { text }) if text == "partial")),
            "expected the pre-stall TextDelta to arrive"
        );
        let Some(Err(err)) = events.last() else {
            panic!("expected the stream to end with an error, got {events:?}");
        };
        match err {
            ProviderError::StreamError { reason } => {
                assert!(
                    reason.contains("timed out"),
                    "reason should mention the timeout: {reason}"
                );
            }
            other => panic!("expected StreamError timeout, got {other:?}"),
        }
        assert!(
            RetryPolicy::default().classifies_as_retryable(err),
            "SSE inactivity timeout must classify as retryable"
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

        let provider = OpenAiProvider::with_auth_provider(
            build_config(server.uri(), 0, Duration::from_secs(10)),
            auth_for_provider,
        )
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

        let provider = OpenAiProvider::with_auth_provider(
            build_config(server.uri(), 0, Duration::from_secs(10)),
            auth_for_provider,
        )
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
