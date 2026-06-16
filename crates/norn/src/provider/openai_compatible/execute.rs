//! HTTP execution for OpenAI-compatible Chat Completions.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;

use super::request::build_payload;
use super::sse::ChatCompletionsMapper;
use crate::error::ProviderError;
use crate::provider::auth::AuthProvider;
use crate::provider::debug::DebugDumper;
use crate::provider::events::ProviderEvent;
use crate::provider::openai::rate_limiter::RateLimiter;
use crate::provider::openai::retry_after::parse_retry_after;
use crate::provider::openai::sse::SseParser;
use crate::provider::request::ProviderRequest;

/// Deliberate default matching the existing `OpenAI` provider when no
/// `ProviderConfig::retry_backoff` is supplied.
pub(super) const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// Per-request sender state cloned out of the provider.
pub(super) struct SenderProvider {
    pub(super) client: reqwest::Client,
    pub(super) endpoint: String,
    pub(super) timeout: Duration,
    pub(super) max_retries: u32,
    pub(super) retry_backoff: Duration,
    pub(super) retry_after_ceiling: Option<Duration>,
    pub(super) rate_limiter: Arc<RateLimiter>,
    pub(super) auth_provider: Arc<dyn AuthProvider>,
    pub(super) debug_dump_file: Option<PathBuf>,
}

impl SenderProvider {
    /// Executes one streaming provider request.
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
        let payload = build_payload(&request)?;
        let body =
            serde_json::to_string(&payload).map_err(|err| ProviderError::ResponseParseError {
                reason: format!("failed to serialize chat completions request: {err}"),
            })?;
        let dumper = self.debug_dump_file.as_deref().and_then(DebugDumper::new);
        if let Some(ref dump) = dumper {
            dump.write_request(&self.endpoint, &body);
        }

        let request_start = std::time::Instant::now();
        let response = self.send_with_retries(body).await?;
        if let Some(ref dump) = dumper {
            let status = response.status().as_u16();
            let headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(key, value)| {
                    (
                        key.to_string(),
                        value.to_str().unwrap_or("<binary>").to_owned(),
                    )
                })
                .collect();
            dump.write_response_meta(status, &headers);
        }

        self.consume_stream(response, tx, dumper, request_start)
            .await
    }

    async fn send_with_retries(&self, body: String) -> Result<reqwest::Response, ProviderError> {
        let mut attempts = 0u32;
        let mut auth_retried = false;
        loop {
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
                    return Err(ProviderError::ConnectionFailed {
                        reason: format!(
                            "connection timed out: no response headers within {:.1}s",
                            self.timeout.as_secs_f64()
                        ),
                    });
                }
            };
            let response = result.map_err(|err| {
                if err.is_timeout() {
                    ProviderError::ConnectionFailed {
                        reason: format!("connection timed out: {err}"),
                    }
                } else {
                    ProviderError::ConnectionFailed {
                        reason: format!("request failed: {err}"),
                    }
                }
            })?;
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
                    Err(err) => return Err(err),
                }
            }

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_retry_after)
                    .map(|wait| {
                        self.retry_after_ceiling
                            .map_or(wait, |ceiling| wait.min(ceiling))
                    });
                let wait = retry_after.unwrap_or(self.retry_backoff);
                self.rate_limiter.impose_cooldown(wait).await;
                attempts = attempts.saturating_add(1);
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

            tracing::debug!(
                elapsed_s = send_start.elapsed().as_secs_f64(),
                status = status.as_u16(),
                endpoint = %self.endpoint,
                "chat completions response headers received"
            );
            return Ok(response);
        }
    }

    async fn consume_stream(
        &self,
        response: reqwest::Response,
        tx: tokio::sync::mpsc::Sender<Result<ProviderEvent, ProviderError>>,
        dumper: Option<DebugDumper>,
        request_start: std::time::Instant,
    ) -> Result<(), ProviderError> {
        let mut parser = SseParser::new();
        let mut mapper = ChatCompletionsMapper::default();
        let mut stream = response.bytes_stream();
        let mut saw_done = false;
        let mut chunk_count = 0u64;
        let mut event_count = 0u64;
        let mut last_chunk = std::time::Instant::now();

        loop {
            let Ok(next) = tokio::time::timeout(self.timeout, stream.next()).await else {
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
            let chunk = chunk_result.map_err(|err| ProviderError::StreamInterrupted {
                reason: format!("connection lost mid-stream: {err}"),
            })?;
            chunk_count = chunk_count.saturating_add(1);
            last_chunk = std::time::Instant::now();
            for sse_event in parser.feed(chunk.as_ref()) {
                event_count = event_count.saturating_add(1);
                if emit_mapped(&mut mapper, &tx, dumper.as_ref(), &sse_event, &mut saw_done).await?
                {
                    tracing::debug!(
                        total_s = request_start.elapsed().as_secs_f64(),
                        chunks = chunk_count,
                        events = event_count,
                        "chat completions request complete"
                    );
                    return Ok(());
                }
            }
        }

        for sse_event in parser.finish() {
            event_count = event_count.saturating_add(1);
            if emit_mapped(&mut mapper, &tx, dumper.as_ref(), &sse_event, &mut saw_done).await? {
                return Ok(());
            }
        }

        if saw_done {
            return Ok(());
        }
        Err(ProviderError::StreamInterrupted {
            reason: format!(
                "chat completions stream ended before terminal finish_reason; chunks={chunk_count}, events={event_count}, last_chunk_age_s={:.1}",
                last_chunk.elapsed().as_secs_f64()
            ),
        })
    }
}

async fn emit_mapped(
    mapper: &mut ChatCompletionsMapper,
    tx: &tokio::sync::mpsc::Sender<Result<ProviderEvent, ProviderError>>,
    dumper: Option<&DebugDumper>,
    sse_event: &crate::provider::openai::sse::SseEvent,
    saw_done: &mut bool,
) -> Result<bool, ProviderError> {
    if let Some(dump) = dumper {
        dump.write_sse_event("chat.completion.chunk", &sse_event.data);
    }
    for event in mapper.map_event(sse_event) {
        let is_done = matches!(event, Ok(ProviderEvent::Done { .. }));
        if tx.send(event).await.is_err() {
            return Ok(true);
        }
        if is_done {
            *saw_done = true;
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::OpenAiCompatibleProvider;
    use crate::provider::auth::{AuthProvider, AuthSource, MockAuthProvider};
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::request::{
        Message, MessageRole, ProviderConfig, ProviderRequest, SecretString,
    };
    use crate::provider::traits::Provider as _;

    fn build_request() -> ProviderRequest {
        ProviderRequest {
            messages: vec![Message {
                role: MessageRole::User,
                content: Some("hello".to_string()),
                thinking: String::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
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
        let auth: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
        OpenAiCompatibleProvider::with_auth_provider(
            ProviderConfig {
                auth_source: AuthSource::ApiKey {
                    key: SecretString::new("unused-direct-auth"),
                },
                base_url: Some(format!("{base_url}/v1")),
                timeout: Duration::from_secs(10),
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
}
