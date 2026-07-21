//! Chat Completions request execution: payload build plus the shared core.
//!
//! All transport behaviour (401 refresh, 429 backoff, error-status
//! handling, SSE consumption) lives in
//! [`StreamExecutor`](crate::provider::exec::StreamExecutor); this module
//! contributes only the Chat Completions payload construction and hands
//! the executor a stateful [`ChatCompletionsMapper`].

use super::request::build_payload;
use super::role_policy::DeveloperRolePolicy;
use super::sse::ChatCompletionsMapper;
use crate::error::ProviderError;
use crate::provider::events::ProviderEvent;
use crate::provider::exec::StreamExecutor;
use crate::provider::request::ProviderRequest;

/// Per-request sender state cloned out of the provider.
pub(super) struct SenderProvider {
    /// Shared transport core.
    pub(super) executor: StreamExecutor,
    /// Provider-pinned developer-message compatibility policy.
    pub(super) developer_role_policy: DeveloperRolePolicy,
}

impl SenderProvider {
    /// Executes one streaming Chat Completions request.
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
        let payload = build_payload(&request, self.developer_role_policy)?;
        let body = serde_json::to_string(&payload).map_err(|err| {
            ProviderError::RequestSerializationFailed {
                reason: format!("failed to serialize chat completions request: {err}"),
            }
        })?;
        tracing::debug!(
            backend = self.executor.backend_label,
            message_count = request.messages.len(),
            "chat completions request starting"
        );
        let mut mapper = ChatCompletionsMapper::default();
        self.executor.execute(body, &mut mapper, &tx).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::OpenAiCompatibleProvider;
    use crate::error::{ErrorClass, ProviderError, TransientKind};
    use crate::provider::auth::{AuthProvider, AuthSource, MockAuthProvider};
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::request::{
        Message, MessageRole, ProviderConfig, ProviderRequest, SecretString,
    };
    use crate::provider::traits::Provider as _;

    fn build_request() -> ProviderRequest {
        ProviderRequest {
            messages: vec![Message {
                response_items: Vec::new(),
                reasoning: Vec::new(),
                role: MessageRole::User,
                content: Some("hello".to_string()),
                thinking: String::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
            }],
            tools: Vec::new(),
            model: "local-chat-model".to_string(),
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

    fn build_provider(base_url: &str) -> OpenAiCompatibleProvider {
        build_provider_with_timeout(base_url, Duration::from_secs(10))
    }

    fn build_provider_with_timeout(base_url: &str, timeout: Duration) -> OpenAiCompatibleProvider {
        let auth: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
        OpenAiCompatibleProvider::with_auth_provider(
            ProviderConfig {
                auth_source: AuthSource::ApiKey {
                    key: SecretString::new("unused-direct-auth"),
                },
                base_url: Some(format!("{base_url}/v1")),
                timeout,
                max_retries: 0,
                provider_options: None,
                debug_dump_file: None,
                rate_limit: None,
                rate_limit_interval: None,
                retry_backoff: None,
                retry_after_ceiling: None,
            },
            auth,
        )
        .expect("provider construction")
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
    async fn stream_posts_chat_request_and_maps_sse_events() {
        let server = MockServer::start().await;
        let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n\
                    data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n\
                    data: [DONE]\n\n";

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let provider = build_provider(&server.uri());
        let mut stream = provider.stream(build_request()).expect("stream");

        let mut text = String::new();
        let mut done = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(ProviderEvent::TextDelta { text: delta }) => text.push_str(&delta),
                Ok(event @ ProviderEvent::Done { .. }) => done = Some(event),
                Ok(_) => {}
                Err(err) => panic!("unexpected provider error: {err}"),
            }
        }

        assert_eq!(text, "hello");
        match done {
            Some(ProviderEvent::Done {
                stop_reason,
                usage,
                response_id,
            }) => {
                assert_eq!(stop_reason, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 7);
                assert_eq!(usage.output_tokens, 2);
                assert!(response_id.is_none());
            }
            other => panic!("expected terminal Done event, got {other:?}"),
        }
    }

    /// Regression (Wave-6 provider finding): under
    /// `stream_options.include_usage` a conformant backend streams the token
    /// usage in a SEPARATE final chunk (empty `choices`, populated `usage`)
    /// AFTER the chunk carrying `finish_reason`, before `[DONE]`. The
    /// terminal `Done` must be deferred so that trailing usage chunk is
    /// consumed and attributed — not reported as zero tokens.
    #[tokio::test]
    async fn stream_attributes_usage_chunk_after_finish_reason() {
        let server = MockServer::start().await;
        let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n\
                    data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                    data: {\"choices\":[],\"usage\":{\"prompt_tokens\":128,\"completion_tokens\":64}}\n\n\
                    data: [DONE]\n\n";

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let provider = build_provider(&server.uri());
        let mut stream = provider.stream(build_request()).expect("stream");

        let mut text = String::new();
        let mut done = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(ProviderEvent::TextDelta { text: delta }) => text.push_str(&delta),
                Ok(event @ ProviderEvent::Done { .. }) => done = Some(event),
                Ok(_) => {}
                Err(err) => panic!("unexpected provider error: {err}"),
            }
        }

        assert_eq!(text, "hello");
        match done {
            Some(ProviderEvent::Done {
                stop_reason,
                usage,
                response_id,
            }) => {
                assert_eq!(stop_reason, StopReason::EndTurn);
                assert_eq!(
                    usage.input_tokens, 128,
                    "usage from the trailing chunk must be attributed, not zero",
                );
                assert_eq!(usage.output_tokens, 64);
                assert!(response_id.is_none());
            }
            other => panic!("expected terminal Done event, got {other:?}"),
        }
    }

    /// The no-usage-chunk server shape (`finish_reason` then `[DONE]`, no
    /// trailing usage chunk) must still terminate cleanly with an `EndTurn`
    /// `Done`, never hang or surface `StreamInterrupted`. Absent usage is
    /// legitimate here.
    #[tokio::test]
    async fn stream_terminates_when_no_usage_chunk_follows_finish_reason() {
        let server = MockServer::start().await;
        let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n\
                    data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                    data: [DONE]\n\n";

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let provider = build_provider(&server.uri());
        let mut stream = provider.stream(build_request()).expect("stream");

        let mut text = String::new();
        let mut done = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(ProviderEvent::TextDelta { text: delta }) => text.push_str(&delta),
                Ok(event @ ProviderEvent::Done { .. }) => done = Some(event),
                Ok(_) => {}
                Err(err) => panic!("unexpected provider error: {err}"),
            }
        }

        assert_eq!(text, "hello");
        match done {
            Some(ProviderEvent::Done {
                stop_reason, usage, ..
            }) => {
                assert_eq!(stop_reason, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 0, "no usage chunk means zero is honest");
                assert_eq!(usage.output_tokens, 0);
            }
            other => panic!("expected terminal Done event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_clean_close_after_text_synthesizes_end_turn() {
        let server = MockServer::start().await;
        let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n\
                    data: [DONE]\n\n";

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let provider = build_provider(&server.uri());
        let mut stream = provider.stream(build_request()).expect("stream");

        let mut text = String::new();
        let mut done = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(ProviderEvent::TextDelta { text: delta }) => text.push_str(&delta),
                Ok(event @ ProviderEvent::Done { .. }) => done = Some(event),
                Ok(_) => {}
                Err(err) => panic!("unexpected provider error: {err}"),
            }
        }

        assert_eq!(text, "hello");
        assert!(matches!(
            done,
            Some(ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                ..
            }),
        ));
    }

    /// A stream that ends before any output and before a `finish_reason`
    /// must surface the retryable `StreamInterrupted` (the shared core's
    /// missing-terminal-event path) rather than ending silently.
    #[tokio::test]
    async fn empty_stream_without_finish_reason_is_stream_interrupted() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(""),
            )
            .expect(1)
            .mount(&server)
            .await;

        let provider = build_provider(&server.uri());
        let mut stream = provider.stream(build_request()).expect("stream");

        let event = stream
            .next()
            .await
            .expect("stream must yield a terminal event");
        match event {
            Err(ProviderError::StreamInterrupted { reason }) => {
                assert!(
                    reason.contains("chunks=") && reason.contains("events="),
                    "reason must carry chunk/event diagnostics: {reason}"
                );
            }
            other => panic!("expected StreamInterrupted, got {other:?}"),
        }
    }

    /// Compatible endpoints drain error bodies only to a sink, preserving the
    /// configured timeout without exposing authority-controlled content.
    #[tokio::test]
    async fn stalled_error_body_times_out_without_exposing_content() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            drain_request(&mut socket).await;
            socket
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\n\
                      Content-Type: text/plain\r\n\
                      Content-Length: 100000\r\n\r\nupstream",
                )
                .await
                .unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(socket);
        });

        let provider = build_provider_with_timeout(
            &format!("http://127.0.0.1:{port}"),
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
                    reason.contains("502"),
                    "reason should surface the HTTP status: {reason}"
                );
                assert_eq!(*transient, Some(TransientKind::Timeout));
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
        assert_eq!(
            err.class(),
            ErrorClass::Retryable {
                kind: TransientKind::Timeout
            },
            "stalled error-body drain must remain a retryable transport timeout"
        );
        assert!(!err.to_string().contains("upstream"));
    }
}
