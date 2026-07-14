//! Responses API request execution: payload build plus the shared core.
//!
//! All transport behaviour (401 refresh, 429 backoff, error-status
//! handling, SSE consumption) lives in
//! [`StreamExecutor`](crate::provider::exec::StreamExecutor); this module
//! contributes only the Responses-specific payload construction and the
//! [`SseEventMapper`] adapter over [`map_sse_event`].

use std::collections::HashMap;

use super::request::build_payload;
use super::sse::{SseEvent, map_sse_event, output_item_added_call_id};
use crate::error::ProviderError;
use crate::provider::events::ProviderEvent;
use crate::provider::exec::{SseEventMapper, StreamExecutor};
use crate::provider::request::ProviderRequest;

/// Per-request sender state cloned out of the provider.
pub(super) struct SenderProvider {
    /// Shared transport core.
    pub(super) executor: StreamExecutor,
    /// Catalog backend identifier for the connection this provider is
    /// actually using (Codex subscription vs. direct Responses API);
    /// governs service-tier resolution in [`build_payload`].
    pub(super) catalog_backend: &'static str,
}

impl SenderProvider {
    /// Executes one streaming Responses API request.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] for serialization, auth, connection, HTTP,
    /// stream, or response-shape failures.
    pub(super) async fn execute(
        &self,
        request: ProviderRequest,
        tx: tokio::sync::mpsc::Sender<Result<ProviderEvent, ProviderError>>,
    ) -> Result<(), ProviderError> {
        let payload = build_payload(&request, self.catalog_backend)?;
        let body = serde_json::to_string(&payload).map_err(|e| {
            ProviderError::RequestSerializationFailed {
                reason: format!("failed to serialize responses request: {e}"),
            }
        })?;
        tracing::debug!(
            backend = self.executor.backend_label,
            message_count = request.messages.len(),
            "responses request starting"
        );
        let mut mapper = ResponsesMapper::default();
        self.executor.execute(body, &mut mapper, &tx).await
    }
}

/// Stateful [`SseEventMapper`] over the Responses API dispatcher.
///
/// The wire dispatch itself is stateless ([`map_sse_event`]); this mapper adds
/// the one piece of per-response state the dispatcher cannot hold: the
/// `item_id` -> `call_id` correlation announced by `response.output_item.added`
/// for tool calls. Each streaming tool-input fragment
/// ([`ProviderEvent::ToolCallDelta`]) is stamped with its `call_id` so an
/// embedder can correlate live input streaming with the tool call its UI knows
/// by `call_id` (the streaming `item_id` is an internal merge key the embedder
/// never sees on the completed call). The map is cleared at response end
/// (terminal `Done`/error) so no correlation leaks across responses.
#[derive(Default)]
struct ResponsesMapper {
    /// Tool-call `item_id` (`fc_*` / `ctc_*`) -> `call_id` (`call_*`),
    /// populated from `response.output_item.added`.
    call_ids: HashMap<String, String>,
}

impl SseEventMapper for ResponsesMapper {
    fn map_event(&mut self, event: &SseEvent) -> Vec<Result<ProviderEvent, ProviderError>> {
        // Record the correlation the instant the item is announced. Per the
        // Responses API lifecycle this `output_item.added` always precedes the
        // item's argument-delta events, so the map is populated before any
        // delta this mapper must stamp.
        if event.event_type == "response.output_item.added"
            && let Some((item_id, call_id)) = output_item_added_call_id(event)
        {
            self.call_ids.insert(item_id, call_id);
        }

        let mut mapped: Vec<Result<ProviderEvent, ProviderError>> =
            map_sse_event(event).into_iter().collect();

        let mut response_ended = false;
        for result in &mut mapped {
            match result {
                Ok(ProviderEvent::ToolCallDelta {
                    item_id, call_id, ..
                }) => {
                    // The stateless dispatcher leaves `call_id` unset; fill it
                    // from the announced correlation. A miss (item never
                    // announced) leaves it `None` — honest, never fabricated.
                    if call_id.is_none() {
                        *call_id = self.call_ids.get(item_id).cloned();
                    }
                }
                Ok(ProviderEvent::Done { .. }) | Err(_) => response_ended = true,
                Ok(_) => {}
            }
        }
        if response_ended {
            self.call_ids.clear();
        }

        mapped
    }

    /// The Responses API always terminates a stream with a
    /// `response.completed`, `response.failed`, or `response.incomplete`
    /// event (each of which maps to a terminal `Done`/`Err`). A byte
    /// stream that ends without one is a transport cutoff, so nothing is
    /// synthesized and the executor surfaces a retryable
    /// [`ProviderError::StreamInterrupted`] with chunk/event diagnostics.
    fn finish_on_clean_close(&mut self) -> Result<Option<ProviderEvent>, ProviderError> {
        Ok(None)
    }

    fn dump_label<'event>(&self, event: &'event SseEvent) -> &'event str {
        &event.event_type
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
    use std::sync::Arc;
    use std::time::Duration;

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

    #[test]
    fn responses_mapper_stamps_call_id_from_output_item_added() {
        // C7: the announced `output_item.added` correlation (item_id -> call_id)
        // is stamped onto the item's subsequent argument-delta events so an
        // embedder can correlate live tool input with the call its UI knows.
        let mut mapper = ResponsesMapper::default();
        let added = SseEvent {
            event_type: "response.output_item.added".to_string(),
            data: serde_json::json!({
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read",
                    "arguments": "",
                }
            }),
        };
        assert!(
            mapper.map_event(&added).is_empty(),
            "output_item.added itself maps to no ProviderEvent",
        );
        let delta = SseEvent {
            event_type: "response.function_call_arguments.delta".to_string(),
            data: serde_json::json!({"item_id": "fc_1", "delta": "{\"path\""}),
        };
        match mapper.map_event(&delta).as_slice() {
            [
                Ok(ProviderEvent::ToolCallDelta {
                    item_id, call_id, ..
                }),
            ] => {
                assert_eq!(item_id, "fc_1");
                assert_eq!(
                    call_id.as_deref(),
                    Some("call_1"),
                    "call_id must be stamped"
                );
            }
            other => panic!("expected a stamped ToolCallDelta, got {other:?}"),
        }

        // Terminal Done clears the map: a stray delta for the same item on a
        // fresh response is no longer correlated (no cross-response leakage).
        let done = SseEvent {
            event_type: "response.completed".to_string(),
            data: serde_json::json!({
                "response": {"status": "completed", "usage": {"input_tokens": 1, "output_tokens": 1}}
            }),
        };
        let _ = mapper.map_event(&done);
        match mapper.map_event(&delta).as_slice() {
            [Ok(ProviderEvent::ToolCallDelta { call_id, .. })] => {
                assert_eq!(*call_id, None, "the map is cleared at response end");
            }
            other => panic!("expected an uncorrelated ToolCallDelta, got {other:?}"),
        }
    }

    #[test]
    fn responses_mapper_delta_without_added_is_uncorrelated() {
        // A delta whose item was never announced carries `call_id: None` —
        // honest, never fabricated.
        let mut mapper = ResponsesMapper::default();
        let delta = SseEvent {
            event_type: "response.function_call_arguments.delta".to_string(),
            data: serde_json::json!({"item_id": "fc_x", "delta": "{"}),
        };
        match mapper.map_event(&delta).as_slice() {
            [Ok(ProviderEvent::ToolCallDelta { call_id, .. })] => {
                assert_eq!(*call_id, None);
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
    }

    fn build_request() -> ProviderRequest {
        ProviderRequest {
            messages: vec![Message {
                reasoning: Vec::new(),
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
            service_tier: None,
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
    async fn incomplete_stream_completes_with_truncation_stop_not_error() {
        // BLOCKER regression: a stream cut by `response.incomplete`
        // (max_output_tokens) must complete normally — accumulated text
        // deltas delivered, terminal Done event carrying
        // `StopReason::MaxTokens` plus the usage and response id from the
        // incomplete payload — and must NOT surface any Err.
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = "event: response.output_text.delta\n\
                    data: {\"delta\":\"partial \"}\n\n\
                    event: response.output_text.delta\n\
                    data: {\"delta\":\"answer\"}\n\n\
                    event: response.incomplete\n\
                    data: {\"response\":{\"id\":\"resp_inc\",\"status\":\"incomplete\",\
                    \"incomplete_details\":{\"reason\":\"max_output_tokens\"},\
                    \"usage\":{\"input_tokens\":11,\"output_tokens\":13}}}\n\n";

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

        let mut text = String::new();
        let mut done: Option<ProviderEvent> = None;
        while let Some(evt) = stream.next().await {
            match evt {
                Ok(ProviderEvent::TextDelta { text: t }) => text.push_str(&t),
                Ok(d @ ProviderEvent::Done { .. }) => done = Some(d),
                Ok(_) => {}
                Err(e) => panic!("truncation must not surface as an error: {e}"),
            }
        }

        assert_eq!(text, "partial answer", "accumulated deltas must survive");
        match done {
            Some(ProviderEvent::Done {
                stop_reason,
                usage,
                response_id,
            }) => {
                assert_eq!(stop_reason, crate::provider::events::StopReason::MaxTokens);
                assert_eq!(usage.input_tokens, 11);
                assert_eq!(usage.output_tokens, 13);
                assert_eq!(response_id.as_deref(), Some("resp_inc"));
            }
            other => panic!("expected a terminal Done event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn incomplete_content_filter_stream_completes_with_typed_stop() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = "event: response.output_text.delta\n\
                    data: {\"delta\":\"redac\"}\n\n\
                    event: response.incomplete\n\
                    data: {\"response\":{\"id\":\"resp_cf\",\"status\":\"incomplete\",\
                    \"incomplete_details\":{\"reason\":\"content_filter\"},\
                    \"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n";

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

        let mut text = String::new();
        let mut stop = None;
        while let Some(evt) = stream.next().await {
            match evt {
                Ok(ProviderEvent::TextDelta { text: t }) => text.push_str(&t),
                Ok(ProviderEvent::Done { stop_reason, .. }) => stop = Some(stop_reason),
                Ok(_) => {}
                Err(e) => panic!("content_filter truncation must not error: {e}"),
            }
        }

        assert_eq!(text, "redac");
        assert_eq!(
            stop,
            Some(crate::provider::events::StopReason::ContentFilter)
        );
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
            ProviderError::ConnectionFailed { reason, kind } => {
                assert!(
                    reason.contains("timed out"),
                    "reason should mention the timeout: {reason}"
                );
                assert_eq!(
                    *kind,
                    crate::error::TransientKind::Timeout,
                    "the structured kind must mark this a timeout"
                );
            }
            other => panic!("expected ConnectionFailed, got {other:?}"),
        }
        assert!(
            RetryPolicy::default().classifies_as_retryable(&err),
            "header-wait timeout must classify as retryable"
        );
    }

    /// A stalled authority-controlled error body is streamed only to a sink:
    /// the configured deadline preserves timeout classification while body
    /// content never reaches the error.
    #[tokio::test]
    async fn stalled_5xx_error_body_times_out_without_exposing_content() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            drain_request(&mut socket).await;
            // Promise a large body, send a fragment, then stall without
            // closing the socket.
            socket
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\n\
                      Content-Type: text/plain\r\n\
                      Content-Length: 100000\r\n\r\noverl",
                )
                .await
                .unwrap();
            socket.flush().await.unwrap();
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
            ProviderError::StreamError { reason, transient } => {
                assert!(
                    reason.contains("timed out"),
                    "reason should mention the timeout: {reason}"
                );
                assert!(
                    reason.contains("503"),
                    "reason should surface the HTTP status: {reason}"
                );
                assert_eq!(*transient, Some(crate::error::TransientKind::Timeout));
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
        assert_eq!(
            err.class(),
            crate::error::ErrorClass::Retryable {
                kind: crate::error::TransientKind::Timeout
            },
            "stalled error-body drain must remain a retryable transport timeout"
        );
        assert!(!err.to_string().contains("overl"));
        assert!(
            RetryPolicy::default().classifies_as_retryable(&err),
            "stalled error-body drain must remain retryable under the default policy"
        );
    }

    /// The 4xx counterpart is also a transport timeout rather than a
    /// deterministic client fault; its body is discarded without disclosure.
    #[tokio::test]
    async fn stalled_4xx_error_body_times_out_without_exposing_content() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            drain_request(&mut socket).await;
            socket
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\n\
                      Content-Type: text/plain\r\n\
                      Content-Length: 100000\r\n\r\nbad",
                )
                .await
                .unwrap();
            socket.flush().await.unwrap();
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
        assert_eq!(
            err.class(),
            crate::error::ErrorClass::Retryable {
                kind: crate::error::TransientKind::Timeout
            }
        );
        assert!(!err.to_string().contains("bad"));
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
            ProviderError::StreamError { reason, transient } => {
                assert!(
                    reason.contains("timed out"),
                    "reason should mention the timeout: {reason}"
                );
                assert_eq!(*transient, Some(crate::error::TransientKind::Timeout));
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

    /// Regression test (final-state hardening, T1 item 2): a Responses
    /// stream that closes *cleanly* (proper chunked terminator) without a
    /// terminal `response.completed`/`failed`/`incomplete` event previously
    /// ended in silence — the provider task returned `Ok(())`, no `Done`
    /// event was emitted, and the loop's fallback classified the condition
    /// as Terminal. The same physical condition on the Chat Completions
    /// path already surfaced as a retryable `StreamInterrupted`. The
    /// Responses provider must now emit its own typed retryable error
    /// carrying chunk/event diagnostics.
    #[tokio::test]
    async fn clean_close_without_terminal_event_yields_retryable_stream_interrupted() {
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
            // Clean chunked-encoding terminator: the HTTP body ends
            // without any transport error and without a terminal event.
            socket.write_all(b"0\r\n\r\n").await.unwrap();
            socket.flush().await.unwrap();
        });

        let provider = build_provider(format!("http://127.0.0.1:{port}"), 0);
        let mut stream = provider.stream(build_request()).expect("stream");

        let mut got_delta = false;
        let mut terminal: Option<Result<ProviderEvent, ProviderError>> = None;
        while let Some(evt) = stream.next().await {
            match evt {
                Ok(ProviderEvent::TextDelta { text }) => {
                    assert_eq!(text, "partial");
                    got_delta = true;
                }
                other => terminal = Some(other),
            }
        }
        server_task.await.unwrap();

        assert!(got_delta, "expected the pre-close TextDelta to arrive");
        let Some(Err(err)) = terminal else {
            panic!("stream must end with a typed error, got {terminal:?}");
        };
        match &err {
            ProviderError::StreamInterrupted { reason } => {
                assert!(
                    reason.contains("terminal event"),
                    "reason must name the missing terminal event: {reason}"
                );
                assert!(
                    reason.contains("chunks=") && reason.contains("events="),
                    "reason must carry chunk/event diagnostics: {reason}"
                );
            }
            other => panic!("expected StreamInterrupted, got {other:?}"),
        }
        assert!(
            RetryPolicy::default().classifies_as_retryable(&err),
            "a stream cut before its terminal event must be retryable"
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
