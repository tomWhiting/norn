//! Shared streaming-request execution core for the SSE-based HTTP providers.
//!
//! Both the `OpenAI` Responses provider and the OpenAI-compatible Chat
//! Completions provider execute requests identically: a send loop that owns
//! 401 refresh-and-retry, 429 `Retry-After` handling, and error-status
//! classification, followed by SSE consumption under an inactivity deadline.
//! The backend-specific pieces are payload construction (done by the caller
//! before invoking [`StreamExecutor::execute`]) and the mapping from parsed
//! SSE frames to [`ProviderEvent`] values (the [`SseEventMapper`]
//! implementation). Everything else lives here exactly once, so retry
//! behaviour and error classification physically cannot diverge between the
//! two backends.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;

use crate::error::{ProviderError, TransientKind};
use crate::provider::auth::AuthProvider;
use crate::provider::debug::DebugDumper;
use crate::provider::events::ProviderEvent;
use crate::provider::openai::rate_limiter::RateLimiter;
use crate::provider::openai::retry_after::parse_retry_after;
use crate::provider::openai::sse::{SseEvent, SseParser};

/// Deliberate, owner-approved default (2026-06-11) used when
/// [`ProviderConfig::retry_backoff`] is `None`: the wait applied to a
/// `429` response that carries no parseable `Retry-After` header.
///
/// [`ProviderConfig::retry_backoff`]: crate::provider::request::ProviderConfig::retry_backoff
pub(crate) const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// Maps parsed SSE frames into provider events for one backend.
///
/// Implementations may be stateful (the Chat Completions mapper accumulates
/// tool-call deltas and usage across frames); the executor drives exactly one
/// mapper instance per request.
pub(crate) trait SseEventMapper {
    /// Maps one parsed SSE frame into zero or more provider events.
    fn map_event(&mut self, event: &SseEvent) -> Vec<Result<ProviderEvent, ProviderError>>;

    /// Called once when the byte stream ended without a terminal
    /// [`ProviderEvent::Done`] having been emitted.
    ///
    /// Returning `Ok(Some(done))` synthesizes a terminal event (Chat
    /// Completions backends that close a text stream cleanly without a
    /// `finish_reason`). Returning `Ok(None)` signals that the backend has
    /// no legitimate way to end a stream without its terminal event, and
    /// the executor surfaces a retryable
    /// [`ProviderError::StreamInterrupted`] carrying chunk/event
    /// diagnostics. Returning `Err` surfaces a backend-specific fault
    /// (e.g. the stream ended with incomplete tool calls).
    fn finish_on_clean_close(&mut self) -> Result<Option<ProviderEvent>, ProviderError>;

    /// Label written to the debug dump for a frame.
    fn dump_label<'event>(&self, event: &'event SseEvent) -> &'event str;
}

/// Per-request execution state cloned out of a provider.
///
/// Owns the full HTTP lifecycle of one streaming call: rate-limiter
/// acquisition, the send/retry loop, and SSE consumption.
pub(crate) struct StreamExecutor {
    /// Shared HTTP client (connection pool) cloned from the provider.
    pub(crate) client: reqwest::Client,
    /// Fully-formed endpoint URL the request is sent to.
    pub(crate) endpoint: String,
    /// Stall deadline from [`ProviderConfig::timeout`]: bounds the wait
    /// for response headers, the gap between SSE chunks, and the read of
    /// an error-response body. Not a whole-request deadline — streams are
    /// legitimately long-lived.
    ///
    /// [`ProviderConfig::timeout`]: crate::provider::request::ProviderConfig::timeout
    pub(crate) timeout: Duration,
    /// In-provider retry budget for `429` responses (see
    /// [`ProviderConfig::max_retries`]).
    ///
    /// [`ProviderConfig::max_retries`]: crate::provider::request::ProviderConfig::max_retries
    pub(crate) max_retries: u32,
    /// Wait applied to a `429` without a parseable `Retry-After` header.
    /// Resolved from [`ProviderConfig::retry_backoff`], falling back to
    /// [`DEFAULT_RETRY_BACKOFF`].
    ///
    /// [`ProviderConfig::retry_backoff`]: crate::provider::request::ProviderConfig::retry_backoff
    pub(crate) retry_backoff: Duration,
    /// Optional ceiling on accepted server-supplied `Retry-After` waits,
    /// from [`ProviderConfig::retry_after_ceiling`]. `None` honors the
    /// header as-is.
    ///
    /// [`ProviderConfig::retry_after_ceiling`]: crate::provider::request::ProviderConfig::retry_after_ceiling
    pub(crate) retry_after_ceiling: Option<Duration>,
    /// Token-bucket limiter shared by every request through this provider.
    pub(crate) rate_limiter: Arc<RateLimiter>,
    /// Authentication applied to each outgoing attempt.
    pub(crate) auth_provider: Arc<dyn AuthProvider>,
    /// JSONL debug-dump target, when configured.
    pub(crate) debug_dump_file: Option<PathBuf>,
    /// Human-readable backend label used in traces and diagnostics
    /// (e.g. `"responses"`, `"chat completions"`).
    pub(crate) backend_label: &'static str,
}

struct GovernedResponse {
    response: reqwest::Response,
    _permit: crate::resource::DescriptorPermit,
}

impl StreamExecutor {
    /// Executes one streaming provider request with a pre-serialized body.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] for auth, connection, HTTP-status, stream,
    /// or response-shape failures.
    pub(crate) async fn execute<M: SseEventMapper>(
        &self,
        body: String,
        mapper: &mut M,
        tx: &tokio::sync::mpsc::Sender<Result<ProviderEvent, ProviderError>>,
    ) -> Result<(), ProviderError> {
        let dumper = self.debug_dump_file.as_deref().and_then(DebugDumper::new);
        if let Some(ref dump) = dumper {
            dump.write_request(&self.endpoint, &body);
        }

        let request_start = std::time::Instant::now();
        tracing::debug!(backend = self.backend_label, "provider request starting");

        let response = self.send_with_retries(&body).await?;

        if let Some(ref dump) = dumper {
            let status = response.response.status().as_u16();
            let headers: Vec<(String, String)> = response
                .response
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

        self.consume_stream(response, mapper, tx, dumper, request_start)
            .await
    }

    /// Sends the request, retrying through 401 refresh and 429 backoff.
    async fn send_with_retries(&self, body: &str) -> Result<GovernedResponse, ProviderError> {
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
                .body(body.to_owned());
            builder = self.auth_provider.apply_auth(builder).await?;
            let governor = crate::resource::DescriptorGovernor::global()
                .map_err(|error| ProviderError::DescriptorAdmission(Box::new(error)))?;
            let permit = governor
                .try_acquire(crate::resource::HTTP_REQUEST_PEAK)
                .map_err(|error| ProviderError::DescriptorAdmission(Box::new(error)))?;

            let result = match tokio::time::timeout(self.timeout, builder.send()).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    tracing::warn!(
                        elapsed_s = send_start.elapsed().as_secs_f64(),
                        timeout_s = self.timeout.as_secs_f64(),
                        backend = self.backend_label,
                        "provider request timed out waiting for response headers"
                    );
                    return Err(ProviderError::ConnectionFailed {
                        reason: format!(
                            "connection timed out: no response headers within {:.1}s",
                            self.timeout.as_secs_f64()
                        ),
                        kind: TransientKind::Timeout,
                    });
                }
            };

            let response = match result {
                Ok(resp) => {
                    tracing::debug!(
                        elapsed_s = send_start.elapsed().as_secs_f64(),
                        status = resp.status().as_u16(),
                        backend = self.backend_label,
                        "provider response headers received"
                    );
                    resp
                }
                Err(error) => {
                    let is_timeout = error.is_timeout();
                    let is_connect = error.is_connect();
                    let error = error.without_url();
                    tracing::warn!(
                        elapsed_s = send_start.elapsed().as_secs_f64(),
                        is_timeout,
                        is_connect,
                        error = %error,
                        backend = self.backend_label,
                        "provider request failed"
                    );
                    if is_timeout {
                        return Err(ProviderError::ConnectionFailed {
                            reason: format!("connection timed out: {error}"),
                            kind: TransientKind::Timeout,
                        });
                    }
                    return Err(ProviderError::ConnectionFailed {
                        reason: format!("request failed: {error}"),
                        kind: TransientKind::ConnectionReset,
                    });
                }
            };

            let response = GovernedResponse {
                response,
                _permit: permit,
            };
            let status = response.response.status();

            if status == reqwest::StatusCode::UNAUTHORIZED {
                if auth_retried {
                    return Err(ProviderError::AuthenticationFailed {
                        reason: "HTTP 401 Unauthorized after token refresh".to_string(),
                    });
                }
                auth_retried = true;
                drop(response);
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
                    .response
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
                drop(response);

                // Impose back-pressure on every caller sharing this
                // limiter for the server-requested window; the gate
                // expires on its own so throughput decays back to the
                // configured baseline. Applied even when this request
                // is out of retries: the server's signal still governs
                // every other in-flight caller.
                self.rate_limiter.impose_cooldown(wait).await;

                attempts = attempts.saturating_add(1);
                if attempts > self.max_retries {
                    return Err(ProviderError::RateLimited { retry_after });
                }

                tokio::time::sleep(wait).await;
                continue;
            }

            if !status.is_success() {
                return Err(self.error_response_to_provider_error(response).await);
            }

            break Ok(response);
        }
    }

    /// Discards a non-2xx response before structural classification.
    ///
    /// The authority-controlled body is never buffered, logged, persisted, or
    /// copied into an error because it may echo prompts, tool content, or
    /// received credentials. Redirects are classified immediately and the
    /// response is dropped, because redirect policy is established entirely by
    /// the status and a stalled body must not disguise that refusal. Other error
    /// bodies stream to a sink under the configured deadline: a server that sends
    /// error headers and then stalls is a retryable timeout, while a completed
    /// body classifies by status.
    async fn error_response_to_provider_error(&self, response: GovernedResponse) -> ProviderError {
        let GovernedResponse { response, _permit } = response;
        let status = response.status();
        if status.is_redirection() {
            return ProviderError::RedirectPolicyRefused {
                status: status.as_u16(),
                backend: self.backend_label,
            };
        }
        let mut body = response.bytes_stream();
        let drain = async {
            while let Some(chunk) = body.next().await {
                chunk?;
            }
            Ok::<(), reqwest::Error>(())
        };
        match tokio::time::timeout(self.timeout, drain).await {
            Err(_) => {
                return ProviderError::StreamError {
                    reason: format!(
                        "reading HTTP {status} error body timed out after {:.1}s",
                        self.timeout.as_secs_f64()
                    ),
                    transient: Some(TransientKind::Timeout),
                };
            }
            Ok(Err(error)) => {
                let is_timeout = error.is_timeout();
                let is_body = error.is_body();
                let error = error.without_url();
                tracing::warn!(
                    status = status.as_u16(),
                    is_timeout,
                    is_body,
                    backend = self.backend_label,
                    error = %error,
                    "failed while discarding provider error-response body"
                );
            }
            Ok(Ok(())) => {}
        }
        ProviderError::StreamError {
            reason: format!(
                "HTTP {status} from {}; response body omitted",
                self.backend_label
            ),
            transient: status
                .is_server_error()
                .then_some(TransientKind::ServerError {
                    status: status.as_u16(),
                }),
        }
    }

    /// Consumes the SSE body, forwarding mapped events until the terminal
    /// event, receiver drop, or a transport fault.
    async fn consume_stream<M: SseEventMapper>(
        &self,
        response: GovernedResponse,
        mapper: &mut M,
        tx: &tokio::sync::mpsc::Sender<Result<ProviderEvent, ProviderError>>,
        dumper: Option<DebugDumper>,
        request_start: std::time::Instant,
    ) -> Result<(), ProviderError> {
        let GovernedResponse { response, _permit } = response;
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
                    backend = self.backend_label,
                    "SSE stream inactivity deadline expired"
                );
                return Err(ProviderError::StreamError {
                    reason: format!(
                        "SSE stream timed out: no data received for {:.1}s",
                        self.timeout.as_secs_f64()
                    ),
                    transient: Some(TransientKind::Timeout),
                });
            };
            let Some(chunk_result) = next else {
                break;
            };
            let chunk = chunk_result.map_err(|error| {
                let error = error.without_url();
                tracing::warn!(
                    stream_elapsed_s = stream_start.elapsed().as_secs_f64(),
                    since_last_chunk_s = last_chunk.elapsed().as_secs_f64(),
                    chunks_received = chunk_count,
                    events_parsed = event_count,
                    error = %error,
                    backend = self.backend_label,
                    "SSE stream interrupted"
                );
                ProviderError::StreamInterrupted {
                    reason: format!("connection lost mid-stream: {error}"),
                }
            })?;
            chunk_count = chunk_count.saturating_add(1);
            last_chunk = std::time::Instant::now();
            for sse_event in parser.feed(chunk.as_ref()) {
                event_count = event_count.saturating_add(1);
                if emit_mapped(mapper, &sse_event, dumper.as_ref(), tx).await {
                    self.log_complete(request_start, stream_start, chunk_count, event_count);
                    return Ok(());
                }
            }
        }

        for sse_event in parser.finish() {
            event_count = event_count.saturating_add(1);
            if emit_mapped(mapper, &sse_event, dumper.as_ref(), tx).await {
                self.log_complete(request_start, stream_start, chunk_count, event_count);
                return Ok(());
            }
        }

        // The byte stream ended without a terminal Done. Let the mapper
        // synthesize one when the backend legitimately closes streams this
        // way; otherwise the physical condition is a mid-conversation
        // transport cutoff and must surface as a retryable interruption,
        // never as a silent success.
        match mapper.finish_on_clean_close()? {
            Some(done) => {
                tracing::debug!(
                    total_s = request_start.elapsed().as_secs_f64(),
                    chunks = chunk_count,
                    events = event_count,
                    backend = self.backend_label,
                    "stream closed cleanly without a terminal event; synthesized completion"
                );
                if tx.send(Ok(done)).await.is_err() {
                    return Ok(());
                }
                Ok(())
            }
            None => Err(ProviderError::StreamInterrupted {
                reason: format!(
                    "{} stream ended before its terminal event; chunks={chunk_count}, events={event_count}, last_chunk_age_s={:.1}",
                    self.backend_label,
                    last_chunk.elapsed().as_secs_f64()
                ),
            }),
        }
    }

    fn log_complete(
        &self,
        request_start: std::time::Instant,
        stream_start: std::time::Instant,
        chunks: u64,
        events: u64,
    ) {
        tracing::debug!(
            total_s = request_start.elapsed().as_secs_f64(),
            stream_s = stream_start.elapsed().as_secs_f64(),
            chunks,
            events,
            backend = self.backend_label,
            "provider request complete"
        );
    }
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;

/// Dumps and maps one SSE frame, forwarding the mapped events.
///
/// Returns `true` when the request is finished: either the terminal `Done`
/// event was delivered, or the receiver dropped (the consumer stopped
/// listening, which ends the request without error).
async fn emit_mapped<M: SseEventMapper>(
    mapper: &mut M,
    sse_event: &SseEvent,
    dumper: Option<&DebugDumper>,
    tx: &tokio::sync::mpsc::Sender<Result<ProviderEvent, ProviderError>>,
) -> bool {
    if let Some(dump) = dumper {
        dump.write_sse_event(mapper.dump_label(sse_event), &sse_event.data);
    }
    for provider_event in mapper.map_event(sse_event) {
        let is_done = matches!(provider_event, Ok(ProviderEvent::Done { .. }));
        if tx.send(provider_event).await.is_err() {
            return true;
        }
        if is_done {
            return true;
        }
    }
    false
}
