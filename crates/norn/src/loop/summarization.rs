//! LLM-written compaction summaries (fix campaign Track L, finding 1).
//!
//! When auto-compaction fires, the loop asks the step's own provider and
//! model to write a semantic summary of the events about to be elided, so
//! the model retains continuity (objectives, decisions, tool outcomes,
//! open work) instead of only the mechanical event digest. The request is
//! a plain, untooled, unthreaded completion: the elided span is rendered
//! to a labelled transcript and sent alongside fixed summarization
//! instructions.
//!
//! Failure policy lives in the caller ([`super::compaction`]): a failed or
//! unusable summarization response is logged and the mechanical digest is
//! committed instead, explicitly marked as a non-semantic fallback.
//!
//! The call runs under the loop's own retry brain (design D11). It used to
//! be the one provider call in the step with zero retries — and it runs in
//! the request-build preflight, *before* the retry brain gets control, so a
//! transient `5xx` during auto-compaction degraded the model's continuity
//! to a mechanical digest for no better reason than a bad minute on the
//! backend. Now a transient failure retries indefinitely under the step's
//! [`RetryPolicy`] and cancellation token, exactly like every other
//! provider call; the digest fallback stays for the failures retrying
//! cannot fix (non-transient errors, truncated or empty responses).

use std::sync::atomic::{AtomicU32, Ordering};

use tokio_util::sync::CancellationToken;

use crate::error::NornError;
use crate::r#loop::classify::{ProviderCallSinks, broadcast_retry_notice, call_provider};
use crate::r#loop::retry::{RetryOutcome, RetryPolicy, retry_with_backoff};
use crate::provider::agent_event::AgentEventSender;
use crate::provider::events::StopReason;
use crate::provider::request::{Message, MessageRole, ProviderRequest};
use crate::provider::traits::Provider;
use crate::provider::usage::Usage;
use crate::session::conversion::prompt_events_to_messages;
use crate::session::events::SessionEvent;

/// Instructions sent as the system message of every summarization request.
const SUMMARIZATION_SYSTEM_PROMPT: &str = "You write compaction summaries for an \
agent conversation. The transcript you receive is the OLDER portion of an ongoing \
conversation; it is about to be removed from the agent's context and replaced by \
your summary. Write a factual, specific summary that preserves everything a \
successor needs to continue seamlessly: the user's objectives and constraints, \
decisions made and their reasons, key facts and values discovered, tools that \
were run and what they returned or changed, errors encountered and how they were \
resolved, and any unfinished work or open questions. Use concrete names, paths, \
identifiers, and numbers from the transcript. Do not add commentary about the \
summarization task itself; output only the summary.";

/// Instruction appended after the transcript in the user message.
const SUMMARIZATION_USER_SUFFIX: &str = "Summarize the conversation transcript \
above. The summary will replace the transcript in the agent's context.";

/// The assembled result of a summarization completion, before the caller
/// has judged whether it is usable.
#[derive(Debug)]
pub(super) struct SummarizationResponse {
    /// Full text the model produced (may be empty).
    pub(super) text: String,
    /// Token usage of the summarization call. Accounted by the caller
    /// even when the response is rejected — the tokens were spent.
    pub(super) usage: Usage,
    /// How the model stopped; anything other than
    /// [`StopReason::EndTurn`] means the summary is incomplete.
    pub(super) stop_reason: StopReason,
}

impl SummarizationResponse {
    /// A summary is usable when the model finished its turn and produced
    /// non-whitespace text. Truncated (`MaxTokens`/`ContentFilter`) or
    /// empty responses must not silently replace conversation history.
    pub(super) fn usable_summary(&self) -> Option<&str> {
        let trimmed = self.text.trim();
        if trimmed.is_empty() || self.stop_reason != StopReason::EndTurn {
            return None;
        }
        Some(trimmed)
    }
}

/// The retry brain's inputs for one summarization call: the step's own
/// policy, its cancellation token, and the live event channel the retry
/// markers are broadcast on.
#[derive(Clone, Copy)]
pub(super) struct SummarizationRetry<'a> {
    /// The step's retry policy — the same one every other provider call
    /// in the step obeys.
    pub(super) policy: &'a RetryPolicy,
    /// The step's cooperative cancellation token. Without one the
    /// inter-attempt wait is uninterruptible by design, so the callers
    /// that host this unbounded loop supply a real token.
    pub(super) cancel: Option<&'a CancellationToken>,
    /// Live agent-event channel for the enriched `AgentStreamRetry`
    /// markers. This is *only* the retry marker: the summarization's own
    /// stream deltas stay unbroadcast (see [`request_compaction_summary`]).
    pub(super) event_tx: Option<&'a AgentEventSender>,
}

/// Terminal outcome of a summarization call, mirroring
/// [`RetryOutcome`] so cancellation stays a first-class outcome rather
/// than being flattened into a failure.
pub(super) enum SummarizationOutcome {
    /// The provider produced an assembled response. Whether it is
    /// *usable* is the caller's judgement
    /// ([`SummarizationResponse::usable_summary`]).
    Completed(SummarizationResponse),
    /// The step's cancellation token fired before or during the call.
    /// Nothing was produced and no further attempt was made; the caller
    /// must end the step as cancelled — never commit a compaction and
    /// never report a provider failure.
    Cancelled,
    /// The call failed with an error no retry can fix (the retry brain
    /// already exhausted every attempt that could have helped).
    Failed(NornError),
}

/// Ask `provider`/`model` to summarize the events about to be elided.
///
/// The request is deliberately isolated from the step's conversation
/// shaping: no tools, no response threading (`previous_response_id` unset,
/// `store` false), no cache key, and no reasoning overrides — every knob
/// defers to the provider's own defaults.
///
/// Transient failures retry under `retry.policy` until the call succeeds,
/// fails non-transiently, or `retry.cancel` fires (design D11). Each wait
/// is announced on `retry.event_tx` through the same enriched
/// [`AgentStreamRetry`](crate::provider::agent_event::AgentStreamRetry)
/// marker the main provider call uses, so a stalled compaction is visible
/// instead of looking like a hang.
pub(super) async fn request_compaction_summary(
    provider: &dyn Provider,
    model: &str,
    elided: &[SessionEvent],
    retry: SummarizationRetry<'_>,
) -> SummarizationOutcome {
    let transcript = render_transcript(elided);
    let request = ProviderRequest {
        messages: vec![
            text_message(MessageRole::System, SUMMARIZATION_SYSTEM_PROMPT.to_string()),
            text_message(
                MessageRole::User,
                format!("{transcript}\n\n{SUMMARIZATION_USER_SUFFIX}"),
            ),
        ],
        tools: Vec::new(),
        model: model.to_string(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    };
    // The sinks' event_tx is deliberately None: streaming summarization
    // deltas to observers would be indistinguishable from assistant
    // output. The partial capture is None for the same reason — a
    // hard-cut summarization call is not assistant output and must not
    // be persisted as the step's partial content. The retry markers do
    // reach observers, on `retry.event_tx` below: a wait nobody can see
    // is exactly the silent stall D8 forbids.
    let sinks = ProviderCallSinks {
        event_tx: None,
        partial_capture: None,
        audio_store: None,
    };
    let sinks = &sinks;
    let attempts = AtomicU32::new(0);
    let outcome = retry_with_backoff(
        retry.policy,
        retry.cancel,
        |notice| broadcast_retry_notice(retry.event_tx, notice),
        || {
            // Each attempt replays the frozen request: the provider's
            // stream consumes it. No audio slot is passed because the
            // summarization sinks carry no audio store, so an abandoned
            // attempt leaves no unsealed sidecar to discard.
            let request = request.clone();
            let attempt = attempts.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            async move {
                call_provider(
                    provider,
                    request,
                    crate::provider::ProviderTurnContext::default(),
                    sinks,
                    attempt,
                    None,
                )
                .await
            }
        },
    )
    .await;
    match outcome {
        RetryOutcome::Completed(response) => {
            SummarizationOutcome::Completed(SummarizationResponse {
                text: response.text,
                usage: response.usage,
                stop_reason: response.stop_reason,
            })
        }
        RetryOutcome::Cancelled => SummarizationOutcome::Cancelled,
        RetryOutcome::Failed(error) => SummarizationOutcome::Failed(error),
    }
}

/// Render the elided events to a labelled plain-text transcript.
///
/// Uses the same event-to-message projection as prompt construction
/// ([`prompt_events_to_messages`]) so tool-call arguments, tool results,
/// and prior compaction summaries appear exactly as the model originally
/// saw them, then flattens each message to a role-labelled block.
pub(super) fn render_transcript(elided: &[SessionEvent]) -> String {
    let messages = prompt_events_to_messages(elided);
    let mut transcript = String::new();
    for message in &messages {
        if !transcript.is_empty() {
            transcript.push_str("\n\n");
        }
        transcript.push_str(&render_message(message));
    }
    transcript
}

fn render_message(message: &Message) -> String {
    let label = match message.role {
        MessageRole::System => "System",
        MessageRole::Developer => "Context note",
        MessageRole::User => "User",
        MessageRole::Assistant => "Assistant",
        MessageRole::ToolResult => "Tool result",
    };
    let mut block = String::from(label);
    if let (MessageRole::ToolResult, Some(name)) = (&message.role, message.tool_name.as_deref()) {
        block.push_str(" (");
        block.push_str(name);
        block.push(')');
    }
    block.push(':');
    if !message.response_items.is_empty() {
        for entry in &message.response_items {
            block.push_str("\n[response item: ");
            block.push_str(entry.item.item_type());
            block.push_str("]\n");
            block.push_str(&entry.item.raw().to_string());
        }
        return block;
    }
    if let Some(content) = message.content.as_deref()
        && !content.is_empty()
    {
        block.push('\n');
        block.push_str(content);
    }
    for call in &message.tool_calls {
        block.push_str("\n[tool call] ");
        block.push_str(&call.name);
        block.push('(');
        block.push_str(&call.arguments);
        block.push(')');
    }
    block
}

fn text_message(role: MessageRole, content: String) -> Message {
    Message {
        response_items: Vec::new(),
        role,
        content: Some(content),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
        tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use futures_util::stream;

    use super::*;
    use crate::error::{ProviderError, TransientKind};
    use crate::provider::events::ProviderEvent;
    use crate::provider::mock::MockProvider;
    use crate::provider::request::ProviderRequest;
    use crate::provider::traits::ProviderStream;
    use crate::session::events::{EventBase, EventUsage, ToolCallEvent};

    /// Retry inputs for a call that is not expected to retry at all.
    fn no_retry_needed() -> SummarizationRetry<'static> {
        static POLICY: std::sync::LazyLock<RetryPolicy> =
            std::sync::LazyLock::new(|| RetryPolicy {
                jitter: false,
                ..RetryPolicy::default()
            });
        SummarizationRetry {
            policy: &POLICY,
            cancel: None,
            event_tx: None,
        }
    }

    /// Scripted provider whose `stream()` calls pop pre-built
    /// `Result<ProviderEvent, ProviderError>` sequences, so a test can
    /// script transport-level failures mid-stream (which
    /// [`MockProvider`] cannot).
    struct ScriptedResultProvider {
        attempts: Mutex<Vec<Vec<Result<ProviderEvent, ProviderError>>>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ScriptedResultProvider {
        fn new(attempts: Vec<Vec<Result<ProviderEvent, ProviderError>>>) -> Self {
            Self {
                attempts: Mutex::new(attempts),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Provider for ScriptedResultProvider {
        fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut attempts = self.attempts.lock().expect("scripted provider lock");
            if attempts.is_empty() {
                return Err(ProviderError::StreamError {
                    reason: "scripted provider exhausted".to_owned(),
                    transient: None,
                });
            }
            let events = attempts.remove(0);
            Ok(Box::pin(stream::iter(events)))
        }
    }

    fn transient_failure() -> Vec<Result<ProviderEvent, ProviderError>> {
        vec![Err(ProviderError::StreamError {
            reason: "HTTP 503: backend having a moment".to_owned(),
            transient: Some(TransientKind::ServerError { status: 503 }),
        })]
    }

    fn summary_attempt(text: &str) -> Vec<Result<ProviderEvent, ProviderError>> {
        vec![
            Ok(ProviderEvent::TextDelta {
                text: text.to_owned(),
            }),
            Ok(ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 12,
                    output_tokens: 3,
                    ..Usage::default()
                },
                response_id: None,
            }),
        ]
    }

    fn user_event(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        }
    }

    fn assistant_with_call(content: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: content.to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "/tmp/a"}),
                kind: crate::provider::request::ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
            }],
            usage: EventUsage::default(),
            stop_reason: "tool_use".to_string(),
            response_id: None,
        }
    }

    fn tool_result_event() -> SessionEvent {
        SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: "call_1".to_string(),
            tool_name: "read".to_string(),
            output: serde_json::json!({"content": "file body"}),
            spool_ref: None,
            duration_ms: 3,
        }
    }

    #[test]
    fn transcript_labels_roles_tool_calls_and_results() {
        let events = vec![
            user_event("please read /tmp/a"),
            assistant_with_call("reading now"),
            tool_result_event(),
        ];
        let transcript = render_transcript(&events);
        assert!(
            transcript.contains("User:\nplease read /tmp/a"),
            "{transcript}"
        );
        assert!(
            transcript.contains("Assistant:\nreading now"),
            "{transcript}"
        );
        assert!(transcript.contains("[tool call] read("), "{transcript}");
        assert!(transcript.contains("Tool result (read):"), "{transcript}");
        assert!(transcript.contains("file body"), "{transcript}");
    }

    #[test]
    fn transcript_renders_canonical_items_in_order_without_flat_projections()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::provider::response_item::{
            ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
        };

        let raw_items = [
            serde_json::json!({
                "type": "reasoning",
                "id": "rs_1",
                "summary": [{"type": "summary_text", "text": "considering"}]
            }),
            serde_json::json!({
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "read",
                "arguments": "{\"path\":\"Cargo.toml\"}"
            }),
            serde_json::json!({
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "refusal", "refusal": "not available"}]
            }),
        ];
        let response_items = raw_items
            .iter()
            .cloned()
            .map(|raw| {
                ResponseItem::from_value(raw).map(|item| ResponseTranscriptItem {
                    item,
                    provenance: ResponseStreamProvenance {
                        item_id: Some("stream-only-id".to_string()),
                        ..ResponseStreamProvenance::default()
                    },
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let event = SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            response_items,
            content: "stale flat text".to_string(),
            thinking: "stale flat thinking".to_string(),
            reasoning: Vec::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: "stale_call".to_string(),
                name: "stale_tool".to_string(),
                arguments: serde_json::json!({}),
                kind: crate::provider::request::ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
            }],
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_string(),
            response_id: None,
        };

        let transcript = render_transcript(&[event]);
        let positions = raw_items
            .iter()
            .map(|raw| {
                transcript.find(&raw.to_string()).ok_or_else(|| {
                    std::io::Error::other(format!("canonical raw item was not rendered: {raw}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(!transcript.contains("stale flat text"));
        assert!(!transcript.contains("stale flat thinking"));
        assert!(!transcript.contains("stale_tool"));
        assert!(!transcript.contains("stream-only-id"));
        Ok(())
    }

    #[test]
    fn transcript_renders_prior_compaction_summaries() {
        let events = vec![SessionEvent::Compaction {
            base: EventBase::new(None),
            summary: "earlier summary text".to_string(),
            replaced_event_ids: Vec::new(),
        }];
        let transcript = render_transcript(&events);
        assert!(transcript.contains("earlier summary text"), "{transcript}");
    }

    #[tokio::test]
    async fn request_is_untooled_unthreaded_and_uses_step_model() {
        let provider = MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "a fine summary".to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 42,
                    output_tokens: 7,
                    ..Usage::default()
                },
                response_id: None,
            },
        ]]);
        let events = vec![user_event("hello")];

        let SummarizationOutcome::Completed(response) =
            request_compaction_summary(&provider, "step-model", &events, no_retry_needed()).await
        else {
            panic!("summarization call succeeds");
        };

        assert_eq!(response.usable_summary(), Some("a fine summary"));
        assert_eq!(response.usage.input_tokens, 42);
        assert_eq!(response.usage.output_tokens, 7);

        let requests = provider.requests().expect("requests recorded");
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.model, "step-model");
        assert!(request.tools.is_empty(), "summarization must be untooled");
        assert!(request.previous_response_id.is_none());
        assert!(!request.store);
        assert!(request.cache_key.is_none());
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, MessageRole::System);
        assert!(
            request.messages[1]
                .content
                .as_deref()
                .is_some_and(|c| c.contains("hello")),
            "transcript must be embedded in the user message",
        );
    }

    /// D11: a transient failure inside the compaction summarizer retries
    /// under the step's policy and succeeds — it no longer costs the
    /// model its semantic continuity. Before the wrap this call had zero
    /// retries: the 503 went straight to the caller and the mechanical
    /// digest replaced the conversation.
    #[tokio::test(start_paused = true)]
    async fn transient_summarization_failure_retries_and_succeeds() {
        let provider = ScriptedResultProvider::new(vec![
            transient_failure(),
            summary_attempt("the conversation so far"),
        ]);
        let policy = RetryPolicy {
            initial_backoff: Duration::from_secs(4),
            jitter: false,
            ..RetryPolicy::default()
        };

        let outcome = request_compaction_summary(
            &provider,
            "step-model",
            &[user_event("hello")],
            SummarizationRetry {
                policy: &policy,
                cancel: None,
                event_tx: None,
            },
        )
        .await;

        let SummarizationOutcome::Completed(response) = outcome else {
            panic!("a transient failure must be retried, not surfaced");
        };
        assert_eq!(response.usable_summary(), Some("the conversation so far"));
        assert_eq!(response.usage.input_tokens, 12);
        assert_eq!(
            provider.calls(),
            2,
            "the failed attempt and its replay both really happened",
        );
    }

    /// D11: the retry markers ride the live channel so a compaction
    /// stalled on a flaky backend is visible instead of looking like a
    /// hang — while the summarization's own stream deltas stay off the
    /// channel, where they would be indistinguishable from assistant
    /// output.
    #[tokio::test(start_paused = true)]
    async fn summarization_retry_broadcasts_the_marker_but_not_its_deltas() {
        use crate::provider::agent_event::{AgentEvent, AgentEventKind, AgentEventSender};

        let provider = ScriptedResultProvider::new(vec![
            transient_failure(),
            summary_attempt("the conversation so far"),
        ]);
        let policy = RetryPolicy {
            initial_backoff: Duration::from_secs(4),
            jitter: false,
            ..RetryPolicy::default()
        };
        let (tx, mut rx) = tokio::sync::broadcast::channel::<AgentEvent>(32);
        let sender = AgentEventSender::new(tx, uuid::Uuid::nil(), "root".to_owned());

        let outcome = request_compaction_summary(
            &provider,
            "step-model",
            &[user_event("hello")],
            SummarizationRetry {
                policy: &policy,
                cancel: None,
                event_tx: Some(&sender),
            },
        )
        .await;
        assert!(matches!(outcome, SummarizationOutcome::Completed(_)));

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event.event);
        }
        assert_eq!(events.len(), 1, "only the retry marker is broadcast");
        match &events[0] {
            AgentEventKind::StreamRetry(retry) => {
                assert_eq!(retry.attempt, 2);
                assert_eq!(retry.max_attempts, None, "the default policy is unbounded");
                assert_eq!(retry.delay_ms, 4_000);
                assert_eq!(retry.error_class, "server_error");
            }
            other => panic!("expected the retry marker, got {other:?}"),
        }
    }

    /// D11: cancellation during a summarization retry is a first-class
    /// outcome — never a provider failure, never a swallowed carry-on.
    /// The caller turns it into the step's `Cancelled` result.
    #[tokio::test(start_paused = true)]
    async fn cancellation_during_a_summarization_retry_surfaces_as_cancelled() {
        let provider = ScriptedResultProvider::new(vec![
            transient_failure(),
            summary_attempt("never reached"),
        ]);
        // A minute of backoff against a token that fires in a second:
        // under the paused clock the earlier timer wins deterministically,
        // so the cancel always lands *inside* the inter-attempt wait.
        let policy = RetryPolicy {
            initial_backoff: Duration::from_secs(60),
            jitter: false,
            ..RetryPolicy::default()
        };
        let token = CancellationToken::new();
        let trigger = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            trigger.cancel();
        });

        let outcome = request_compaction_summary(
            &provider,
            "step-model",
            &[user_event("hello")],
            SummarizationRetry {
                policy: &policy,
                cancel: Some(&token),
                event_tx: None,
            },
        )
        .await;

        assert!(
            matches!(outcome, SummarizationOutcome::Cancelled),
            "a cancelled retry wait is cancellation, not failure",
        );
        assert_eq!(
            provider.calls(),
            1,
            "no attempt may start after the token fires",
        );
    }

    /// D11 boundary: a non-transient failure is not retried at all. It
    /// surfaces immediately so the caller can commit the marked
    /// mechanical digest — today's failure policy, unchanged.
    #[tokio::test(start_paused = true)]
    async fn non_transient_summarization_failure_is_reported_without_retrying() {
        let provider = ScriptedResultProvider::new(vec![
            vec![Err(ProviderError::StreamError {
                reason: "malformed response envelope".to_owned(),
                transient: None,
            })],
            summary_attempt("never reached"),
        ]);

        let outcome = request_compaction_summary(
            &provider,
            "step-model",
            &[user_event("hello")],
            no_retry_needed(),
        )
        .await;

        let SummarizationOutcome::Failed(error) = outcome else {
            panic!("a terminal failure must surface, not retry forever");
        };
        assert!(
            error.to_string().contains("malformed response envelope"),
            "{error}",
        );
        assert_eq!(
            provider.calls(),
            1,
            "a terminal error must never be replayed",
        );
    }

    #[test]
    fn truncated_or_empty_summaries_are_unusable() {
        let truncated = SummarizationResponse {
            text: "cut off mid".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::MaxTokens,
        };
        assert!(truncated.usable_summary().is_none());

        let empty = SummarizationResponse {
            text: "   \n".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::EndTurn,
        };
        assert!(empty.usable_summary().is_none());
    }
}
