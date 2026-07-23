//! Responses API request execution: payload build plus the shared core.
//!
//! All transport behaviour (401 refresh, 429 backoff, error-status
//! handling, SSE consumption) lives in
//! [`StreamExecutor`](crate::provider::exec::StreamExecutor); this module
//! contributes only the Responses-specific payload construction and the
//! [`SseEventMapper`] adapter over [`map_sse_event`].

use std::collections::HashMap;

use super::codex_turn;
use super::request::build_payload;
use super::response_reconciler::{
    DeltaReconciliation, ReconcileUpdate, ResponseDeltaChannel, ResponseReconciler,
    ResponseReconciliationError,
};
use super::response_stream_event::{ResponseStreamEvent, ResponseStreamEventManifest};
use super::response_terminal::{ResponsesDialect, decode_terminal};
use super::sse::{SseEvent, map_sse_event, output_item_added_call_id};
use crate::error::ProviderError;
use crate::provider::events::ProviderEvent;
use crate::provider::exec::{SseEventMapper, StreamExecutor};
use crate::provider::request::ProviderRequest;
use crate::provider::response_audio::is_response_audio_event;
use crate::provider::turn::{
    CODEX_TURN_STATE_HEADER, ProviderTurnContext, codex_turn_state_from_metadata,
    redact_codex_turn_state,
};

/// Per-request sender state cloned out of the provider.
pub(super) struct SenderProvider {
    /// Shared transport core.
    pub(super) executor: StreamExecutor,
    /// Catalog backend identifier for the connection this provider is
    /// actually using (Codex subscription vs. direct Responses API);
    /// governs service-tier resolution in [`build_payload`].
    pub(super) catalog_backend: &'static str,
    /// Trusted live-turn transport context for Codex subscription requests.
    pub(super) turn_context: Option<ProviderTurnContext>,
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
        let mut payload = build_payload(&request, self.catalog_backend)?;
        codex_turn::insert_client_metadata(&mut payload, self.turn_context.as_ref())?;
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
        let mut mapper =
            ResponsesMapper::with_turn_context(self.catalog_backend, self.turn_context.clone());
        self.executor.execute(body, &mut mapper, &tx).await
    }
}

/// Stateful Responses stream validation, reconciliation, and projection.
///
/// Every valid envelope is emitted for machine observers, with the reusable
/// `x-codex-turn-state` transport secret redacted. The reconciler separately
/// owns identity-keyed preview state and emits canonical items only in the
/// dialect's terminal-authority order: public `response.output`, or Codex
/// completed items ordered by `output_index` when terminal output is absent.
/// [`map_sse_event`] remains a compatibility projection for the existing
/// text/thinking/tool UI; it is no longer an authority for replay or execution.
#[derive(Default)]
struct ResponsesMapper {
    /// Tool-call `item_id` (`fc_*` / `ctc_*`) -> `call_id` (`call_*`),
    /// populated from `response.output_item.added`.
    call_ids: HashMap<String, String>,
    /// Per-response identity and completion state.
    reconciler: ResponseReconciler,
    /// Whether this mapper already delivered a terminal outcome.
    terminal: bool,
    /// Trusted response dialect selected before request dispatch.
    dialect: ResponsesDialect,
    /// Trusted, non-persisted Codex state for this live turn.
    turn_context: Option<ProviderTurnContext>,
}

impl SseEventMapper for ResponsesMapper {
    fn observe_response_headers(&mut self, headers: &reqwest::header::HeaderMap) {
        let Some(context) = self.turn_context.as_ref() else {
            return;
        };
        if let Some(value) = headers
            .get(CODEX_TURN_STATE_HEADER)
            .and_then(|value| value.to_str().ok())
        {
            context.observe_codex_turn_state(value);
        }
    }

    fn map_event(&mut self, event: &SseEvent) -> Vec<Result<ProviderEvent, ProviderError>> {
        if self.terminal {
            return vec![Err(ProviderError::ResponseProtocolViolation {
                source: ResponseReconciliationError::PostTerminalFrame,
            })];
        }

        let event_data = redact_codex_turn_state(&event.data);
        let envelope = match ResponseStreamEvent::from_sse(&event.event_type, event_data) {
            Ok(envelope) => envelope,
            Err(error) => {
                self.terminal = true;
                return vec![Err(ProviderError::ResponseParseError {
                    reason: error.to_string(),
                })];
            }
        };
        let manifest = envelope.manifest();
        if manifest.known_event_type() == Some("response.metadata")
            && let Some(context) = self.turn_context.as_ref()
            && let Some(value) = codex_turn_state_from_metadata(&event.data)
        {
            context.observe_codex_turn_state(value);
        }
        let audio_source =
            is_response_audio_event(envelope.event_type()).then(|| Box::new(envelope.clone()));
        let mut mapped = vec![Ok(ProviderEvent::ResponseStreamEvent {
            event: Box::new(envelope),
        })];

        match manifest {
            ResponseStreamEventManifest::Unknown => {
                self.finish();
                mapped.push(Err(ProviderError::UnsupportedResponseEvent));
                return mapped;
            }
            ResponseStreamEventManifest::CodexOverlay(_) => return mapped,
            ResponseStreamEventManifest::Public(_) => {}
        }

        let update = match self.reconciler.ingest(event) {
            Ok(update) => update,
            Err(error) => {
                for item in error.retained_items() {
                    mapped.push(Ok(ProviderEvent::ResponseItemDone { item: item.clone() }));
                }
                let provider_error = match &error {
                    ResponseReconciliationError::UnknownOutputItemType { .. }
                    | ResponseReconciliationError::UnsupportedExecutableItem { .. } => {
                        ProviderError::UnsupportedResponseItem
                    }
                    _ => ProviderError::ResponseProtocolViolation { source: error },
                };
                self.finish();
                mapped.push(Err(provider_error));
                return mapped;
            }
        };

        if matches!(
            update,
            ReconcileUpdate::DuplicateSequence { .. }
                | ReconcileUpdate::DuplicateCompletion { .. }
                | ReconcileUpdate::DuplicateChannelCompletion
        ) {
            return mapped;
        }

        if let ReconcileUpdate::ResponseAudio { event } = &update {
            let Some(stream_event) = audio_source else {
                self.finish();
                mapped.push(Err(ProviderError::ResponseProtocolViolation {
                    source: ResponseReconciliationError::UnclassifiedPublicEvent,
                }));
                return mapped;
            };
            mapped.push(Ok(ProviderEvent::ResponseAudioFrame {
                stream_event,
                event: event.clone(),
            }));
        }

        if let Err(error) = append_reconciliation_repairs(&update, &mut mapped) {
            self.finish();
            mapped.push(Err(error));
            return mapped;
        }

        if event.event_type == "response.output_item.added"
            && let Some((item_id, call_id)) = output_item_added_call_id(event)
        {
            self.call_ids.insert(item_id, call_id);
        }

        let terminal_items = match &update {
            ReconcileUpdate::Terminal { items, .. } => Some(items.as_slice()),
            ReconcileUpdate::Accepted
            | ReconcileUpdate::ResponseAudio { .. }
            | ReconcileUpdate::Ignored
            | ReconcileUpdate::DuplicateSequence { .. }
            | ReconcileUpdate::DuplicateCompletion { .. }
            | ReconcileUpdate::DuplicateChannelCompletion
            | ReconcileUpdate::CompletedChannel { .. }
            | ReconcileUpdate::CompletedItem { .. } => None,
        };
        if let Some(items) = terminal_items {
            mapped.extend(
                items
                    .iter()
                    .cloned()
                    .map(|item| Ok(ProviderEvent::ResponseItemDone { item })),
            );
        }

        let projection = if matches!(
            event.event_type.as_str(),
            "response.completed" | "response.incomplete"
        ) {
            Some(decode_terminal(event, &update, self.dialect))
        } else {
            map_sse_event(event)
        };
        if let Some(mut projected) = projection {
            match &mut projected {
                Ok(ProviderEvent::ResponseItemDone { .. }) => {}
                Ok(ProviderEvent::ToolCallDelta {
                    item_id, call_id, ..
                }) => {
                    if call_id.is_none() {
                        *call_id = self.call_ids.get(item_id).cloned();
                    }
                    mapped.push(projected);
                }
                Ok(ProviderEvent::Done { .. }) | Err(_) => {
                    self.finish();
                    mapped.push(projected);
                }
                Ok(_) => mapped.push(projected),
            }
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

impl ResponsesMapper {
    fn new(dialect: ResponsesDialect) -> Self {
        Self {
            call_ids: HashMap::new(),
            reconciler: ResponseReconciler::with_terminal_output_policy(
                dialect.terminal_output_policy(),
            ),
            terminal: false,
            dialect,
            turn_context: None,
        }
    }

    fn for_catalog_backend(catalog_backend: &str) -> Self {
        Self::new(ResponsesDialect::for_catalog_backend(catalog_backend))
    }

    fn with_turn_context(catalog_backend: &str, turn_context: Option<ProviderTurnContext>) -> Self {
        Self {
            turn_context,
            ..Self::for_catalog_backend(catalog_backend)
        }
    }

    fn finish(&mut self) {
        self.terminal = true;
        self.call_ids.clear();
    }
}

fn append_reconciliation_repairs(
    update: &ReconcileUpdate,
    mapped: &mut Vec<Result<ProviderEvent, ProviderError>>,
) -> Result<(), ProviderError> {
    match update {
        ReconcileUpdate::CompletedChannel {
            delta_reconciliation,
        } => append_reconciliation_repair(delta_reconciliation, mapped),
        ReconcileUpdate::CompletedItem {
            delta_reconciliations,
            ..
        }
        | ReconcileUpdate::Terminal {
            delta_reconciliations,
            ..
        } => {
            for reconciliation in delta_reconciliations {
                append_reconciliation_repair(reconciliation, mapped)?;
            }
            Ok(())
        }
        ReconcileUpdate::Accepted
        | ReconcileUpdate::ResponseAudio { .. }
        | ReconcileUpdate::Ignored
        | ReconcileUpdate::DuplicateSequence { .. }
        | ReconcileUpdate::DuplicateCompletion { .. }
        | ReconcileUpdate::DuplicateChannelCompletion => Ok(()),
    }
}

fn append_reconciliation_repair(
    reconciliation: &DeltaReconciliation,
    mapped: &mut Vec<Result<ProviderEvent, ProviderError>>,
) -> Result<(), ProviderError> {
    let Some(repair) = reconciliation.repair.clone() else {
        return Ok(());
    };
    let event = match reconciliation.channel {
        ResponseDeltaChannel::OutputText(_) => ProviderEvent::TextDelta { text: repair },
        ResponseDeltaChannel::Refusal(content_index) => ProviderEvent::RefusalDelta {
            item_id: reconciliation
                .identity
                .item_id()
                .ok_or(ProviderError::ResponseProtocolViolation {
                    source: ResponseReconciliationError::InvalidEnvelopeField {
                        event_type: "response reconciliation",
                        field: "message item id",
                    },
                })?
                .to_owned(),
            output_index: reconciliation.identity.output_index(),
            content_index,
            refusal: repair,
        },
        ResponseDeltaChannel::ReasoningSummaryText(_) | ResponseDeltaChannel::ReasoningText(_) => {
            ProviderEvent::ThinkingDelta { text: repair }
        }
        ResponseDeltaChannel::FunctionCallArguments | ResponseDeltaChannel::CustomToolCallInput => {
            return Ok(());
        }
    };
    mapped.push(Ok(event));
    Ok(())
}

#[cfg(test)]
mod turn_state_tests;

#[cfg(test)]
mod reconciliation_tests;

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
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::openai::OpenAiProvider;
    use crate::provider::openai::request::{
        CATALOG_BACKEND_CODEX_SUBSCRIPTION, CATALOG_BACKEND_RESPONSES_API,
    };
    use crate::provider::request::{
        Message, MessageRole, ProviderConfig, ProviderRequest, SecretString,
    };
    use crate::provider::traits::Provider as _;
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn completed_with_end_turn(end_turn: bool) -> SseEvent {
        SseEvent {
            event_type: "response.completed".to_owned(),
            data: serde_json::json!({
                "type": "response.completed",
                "sequence_number": 0,
                "response": {
                    "id": "resp_end_turn",
                    "status": "completed",
                    "output": [],
                    "end_turn": end_turn,
                    "usage": {
                        "input_tokens": 1,
                        "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
                        "output_tokens": 0,
                        "output_tokens_details": {"reasoning_tokens": 0},
                        "total_tokens": 1
                    }
                }
            }),
        }
    }

    #[test]
    fn mapper_honors_end_turn_only_for_the_codex_dialect() {
        let event = completed_with_end_turn(false);
        let mut codex = ResponsesMapper::new(ResponsesDialect::Codex);
        assert!(codex.map_event(&event).iter().any(|mapped| {
            matches!(
                mapped,
                Ok(ProviderEvent::Done {
                    stop_reason: StopReason::ContinueTurn,
                    ..
                })
            )
        }));

        let mut public = ResponsesMapper::new(ResponsesDialect::Public);
        let public_events = public.map_event(&event);
        assert!(
            public_events
                .iter()
                .any(|mapped| { matches!(mapped, Err(ProviderError::ResponseParseError { .. })) })
        );
        assert!(
            !public_events
                .iter()
                .any(|mapped| { matches!(mapped, Ok(ProviderEvent::Done { .. })) })
        );
    }

    #[test]
    fn trusted_catalog_backend_selects_the_terminal_dialect() {
        assert_eq!(
            ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION).dialect,
            ResponsesDialect::Codex,
        );
        assert_eq!(
            ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_RESPONSES_API).dialect,
            ResponsesDialect::Public,
        );
        assert_eq!(
            ResponsesMapper::for_catalog_backend("unknown_backend").dialect,
            ResponsesDialect::Public,
        );
    }

    #[test]
    fn responses_mapper_stamps_call_id_from_output_item_added() {
        // C7: the announced `output_item.added` correlation (item_id -> call_id)
        // is stamped onto the item's subsequent argument-delta events so an
        // embedder can correlate live tool input with the call its UI knows.
        let mut mapper = ResponsesMapper::default();
        let added = SseEvent {
            event_type: "response.output_item.added".to_string(),
            data: serde_json::json!({
                "type": "response.output_item.added",
                "sequence_number": 0,
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read",
                    "arguments": "",
                    "status": "in_progress"
                }
            }),
        };
        let added_events = mapper.map_event(&added);
        assert!(matches!(
            added_events.as_slice(),
            [Ok(ProviderEvent::ResponseStreamEvent { .. })]
        ));
        let delta = SseEvent {
            event_type: "response.function_call_arguments.delta".to_string(),
            data: serde_json::json!({
                "type": "response.function_call_arguments.delta",
                "sequence_number": 1,
                "item_id": "fc_1",
                "output_index": 0,
                "delta": "{\"path\""
            }),
        };
        match mapper.map_event(&delta).as_slice() {
            [
                Ok(ProviderEvent::ResponseStreamEvent { .. }),
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

        // Terminal completion emits the canonical item before Done and closes
        // the mapper. A later frame is rejected rather than correlated using
        // stale state.
        let done = SseEvent {
            event_type: "response.completed".to_string(),
            data: serde_json::json!({
                "type": "response.completed",
                "sequence_number": 2,
                "response": {
                    "id": "resp_1",
                    "status": "completed",
                    "output": [{
                        "type": "function_call",
                        "id": "fc_1",
                        "call_id": "call_1",
                        "name": "read",
                        "arguments": "{\"path\"}",
                        "status": "completed"
                    }],
                    "usage": {
                        "input_tokens": 1,
                        "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
                        "output_tokens": 1,
                        "output_tokens_details": {"reasoning_tokens": 0},
                        "total_tokens": 2
                    }
                }
            }),
        };
        assert!(matches!(
            mapper.map_event(&done).as_slice(),
            [
                Ok(ProviderEvent::ResponseStreamEvent { .. }),
                Ok(ProviderEvent::ResponseItemDone { .. }),
                Ok(ProviderEvent::Done { .. })
            ]
        ));
        let post_terminal = mapper.map_event(&delta);
        assert!(
            matches!(
                post_terminal.as_slice(),
                [Err(ProviderError::ResponseProtocolViolation { .. })]
            ),
            "expected a post-terminal protocol error, got {post_terminal:?}"
        );
    }

    #[test]
    fn responses_mapper_rejects_delta_without_added_identity() {
        let mut mapper = ResponsesMapper::default();
        let delta = SseEvent {
            event_type: "response.function_call_arguments.delta".to_string(),
            data: serde_json::json!({
                "type": "response.function_call_arguments.delta",
                "sequence_number": 0,
                "item_id": "fc_x",
                "output_index": 0,
                "delta": "{"
            }),
        };
        let mapped = mapper.map_event(&delta);
        assert!(
            matches!(
                mapped.as_slice(),
                [
                    Ok(ProviderEvent::ResponseStreamEvent { .. }),
                    Err(ProviderError::ResponseProtocolViolation { .. }),
                ]
            ),
            "expected raw event then protocol error, got {mapped:?}"
        );
    }

    fn build_request() -> ProviderRequest {
        ProviderRequest {
            messages: vec![Message {
                response_items: Vec::new(),
                reasoning: Vec::new(),
                role: MessageRole::User,
                content: Some("hello".to_string()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
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

    fn sse_frame(event_type: &str, payload: &serde_json::Value) -> String {
        format!("event: {event_type}\ndata: {payload}\n\n")
    }

    fn message_added_frame(sequence_number: u64, item_id: &str) -> String {
        sse_frame(
            "response.output_item.added",
            &serde_json::json!({
                "type": "response.output_item.added",
                "sequence_number": sequence_number,
                "output_index": 0,
                "item": {
                    "id": item_id,
                    "type": "message",
                    "role": "assistant",
                    "status": "in_progress",
                    "content": []
                }
            }),
        )
    }

    fn output_text_delta_frame(sequence_number: u64, item_id: &str, delta: &str) -> String {
        sse_frame(
            "response.output_text.delta",
            &serde_json::json!({
                "type": "response.output_text.delta",
                "sequence_number": sequence_number,
                "item_id": item_id,
                "output_index": 0,
                "content_index": 0,
                "delta": delta,
                "logprobs": []
            }),
        )
    }

    #[derive(Clone, Copy)]
    enum MessageTerminal<'reason> {
        Completed,
        Incomplete(&'reason str),
    }

    fn terminal_message_frame(
        sequence_number: u64,
        response_id: &str,
        item_id: &str,
        text: &str,
        terminal: MessageTerminal<'_>,
        input_tokens: u64,
        output_tokens: u64,
    ) -> String {
        let total_tokens = input_tokens.saturating_add(output_tokens);
        let (event_type, status, item_status, incomplete_details) = match terminal {
            MessageTerminal::Completed => (
                "response.completed",
                "completed",
                "completed",
                serde_json::Value::Null,
            ),
            MessageTerminal::Incomplete(reason) => (
                "response.incomplete",
                "incomplete",
                "incomplete",
                serde_json::json!({"reason": reason}),
            ),
        };
        sse_frame(
            event_type,
            &serde_json::json!({
                "type": event_type,
                "sequence_number": sequence_number,
                "response": {
                    "id": response_id,
                    "status": status,
                    "output": [{
                        "id": item_id,
                        "type": "message",
                        "role": "assistant",
                        "status": item_status,
                        "content": [{
                            "type": "output_text",
                            "text": text,
                            "annotations": [],
                            "logprobs": []
                        }]
                    }],
                    "incomplete_details": incomplete_details,
                    "usage": {
                        "input_tokens": input_tokens,
                        "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
                        "output_tokens": output_tokens,
                        "output_tokens_details": {"reasoning_tokens": 0},
                        "total_tokens": total_tokens
                    }
                }
            }),
        )
    }

    fn completed_message_stream(
        response_id: &str,
        item_id: &str,
        deltas: &[&str],
        input_tokens: u64,
        output_tokens: u64,
    ) -> String {
        let mut body = message_added_frame(0, item_id);
        let mut text = String::new();
        let mut sequence_number = 1;
        for delta in deltas {
            text.push_str(delta);
            body.push_str(&output_text_delta_frame(sequence_number, item_id, delta));
            sequence_number = sequence_number.saturating_add(1);
        }
        body.push_str(&terminal_message_frame(
            sequence_number,
            response_id,
            item_id,
            &text,
            MessageTerminal::Completed,
            input_tokens,
            output_tokens,
        ));
        body
    }

    fn incomplete_message_stream(
        response_id: &str,
        item_id: &str,
        deltas: &[&str],
        reason: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> String {
        let mut body = message_added_frame(0, item_id);
        let mut text = String::new();
        let mut sequence_number = 1;
        for delta in deltas {
            text.push_str(delta);
            body.push_str(&output_text_delta_frame(sequence_number, item_id, delta));
            sequence_number = sequence_number.saturating_add(1);
        }
        body.push_str(&terminal_message_frame(
            sequence_number,
            response_id,
            item_id,
            &text,
            MessageTerminal::Incomplete(reason),
            input_tokens,
            output_tokens,
        ));
        body
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
        let body = completed_message_stream("resp_multi", "msg_multi", &["hello", "world"], 1, 2);

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
        let mut raw_event_types = Vec::new();
        let mut completed_items = 0;
        while let Some(evt) = stream.next().await {
            match evt {
                Ok(ProviderEvent::TextDelta { text }) => deltas.push(text),
                Ok(ProviderEvent::Done { .. }) => got_done = true,
                Ok(ProviderEvent::ResponseStreamEvent { event }) => {
                    raw_event_types.push(event.event_type().to_owned());
                }
                Ok(ProviderEvent::ResponseItemDone { .. }) => completed_items += 1,
                Ok(_) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        assert_eq!(deltas, vec!["hello".to_string(), "world".to_string()]);
        assert!(got_done, "expected Done event");
        assert_eq!(
            raw_event_types,
            [
                "response.output_item.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.completed"
            ],
            "each wire frame must be surfaced exactly once in transport order"
        );
        assert_eq!(
            completed_items, 1,
            "the terminal output item must be emitted exactly once"
        );
    }

    #[tokio::test]
    async fn duplicate_terminal_wire_frames_deliver_one_terminal_outcome()
    -> Result<(), Box<dyn std::error::Error>> {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let terminal = terminal_message_frame(
            0,
            "resp_duplicate_terminal",
            "msg_duplicate_terminal",
            "answer",
            MessageTerminal::Completed,
            7,
            3,
        );
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(format!("{terminal}{terminal}")),
            )
            .mount(&server)
            .await;

        let provider = build_provider(server.uri(), 0);
        let mut stream = provider.stream(build_request())?;
        let mut raw_terminal_count = 0;
        let mut canonical_item_count = 0;
        let mut terminal = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(ProviderEvent::ResponseStreamEvent { event }) => {
                    raw_terminal_count += usize::from(event.event_type() == "response.completed");
                }
                Ok(ProviderEvent::ResponseItemDone { .. }) => canonical_item_count += 1,
                Ok(done @ ProviderEvent::Done { .. }) => terminal = Some(done),
                Ok(_) => {}
                Err(error) => return Err(error.into()),
            }
        }

        assert_eq!(raw_terminal_count, 1);
        assert_eq!(canonical_item_count, 1);
        let Some(ProviderEvent::Done {
            usage, response_id, ..
        }) = terminal
        else {
            return Err(std::io::Error::other("expected exactly one terminal Done event").into());
        };
        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(response_id.as_deref(), Some("resp_duplicate_terminal"));
        Ok(())
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
        let body = incomplete_message_stream(
            "resp_inc",
            "msg_inc",
            &["partial ", "answer"],
            "max_output_tokens",
            11,
            13,
        );

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
        let body =
            incomplete_message_stream("resp_cf", "msg_cf", &["redac"], "content_filter", 5, 2);

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

        let frame1 = format!(
            "{}{}",
            message_added_frame(0, "msg_incremental"),
            output_text_delta_frame(1, "msg_incremental", "a")
        );
        let frame2 = output_text_delta_frame(2, "msg_incremental", "b");
        let frame3 = terminal_message_frame(
            3,
            "resp_incremental",
            "msg_incremental",
            "ab",
            MessageTerminal::Completed,
            1,
            2,
        );
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
        let body = completed_message_stream("resp_retry", "msg_retry", &["after-retry"], 1, 2);

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
        let body = completed_message_stream("resp_clamp", "msg_clamp", &["after-clamp"], 1, 2);

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
        let body = completed_message_stream(
            "resp_far_future",
            "msg_far_future",
            &["after-far-future"],
            1,
            2,
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
        let body = completed_message_stream(
            "resp_fast_backoff",
            "msg_fast_backoff",
            &["after-fast-backoff"],
            1,
            2,
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
        let body = completed_message_stream(
            "resp_date_retry",
            "msg_date_retry",
            &["after-date-retry"],
            1,
            2,
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

        let frame = format!(
            "{}{}",
            message_added_frame(0, "msg_stalled"),
            output_text_delta_frame(1, "msg_stalled", "partial")
        );

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

        let frame = format!(
            "{}{}",
            message_added_frame(0, "msg_dropped"),
            output_text_delta_frame(1, "msg_dropped", "partial")
        );

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
    /// stream that closes *cleanly* (proper chunked terminator) with an
    /// unterminated `response.completed` frame previously
    /// ended in silence — the provider task returned `Ok(())`, no `Done`
    /// event was emitted, and the loop's fallback classified the condition
    /// as Terminal. The same physical condition on the Chat Completions
    /// path already surfaced as a retryable `StreamInterrupted`. The
    /// Responses provider must now emit its own typed retryable error
    /// carrying chunk/event diagnostics.
    #[tokio::test]
    async fn clean_close_with_unterminated_terminal_frame_yields_stream_interrupted() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let mut frame = format!(
            "{}{}",
            message_added_frame(0, "msg_clean_close"),
            output_text_delta_frame(1, "msg_clean_close", "partial")
        );
        let mut unterminated = terminal_message_frame(
            2,
            "resp_clean_close",
            "msg_clean_close",
            "partial",
            MessageTerminal::Completed,
            1,
            1,
        );
        assert_eq!(unterminated.pop(), Some('\n'));
        frame.push_str(&unterminated);

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
            // Clean chunked-encoding terminator. The apparent terminal
            // frame lacks its dispatching blank line, so SSE requires it
            // to be discarded at EOF rather than promoted to success.
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
        let body =
            completed_message_stream("resp_refresh", "msg_refresh", &["after-refresh"], 1, 2);

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

    #[tokio::test]
    async fn malformed_sse_json_yields_one_typed_terminal_error()
    -> Result<(), Box<dyn std::error::Error>> {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        "event: response.created\n\
                         data: {\"type\":\"response.created\",broken}\n\n",
                    ),
            )
            .mount(&server)
            .await;

        let provider = build_provider(server.uri(), 0);
        let mut stream = provider.stream(build_request())?;
        let events: Vec<_> = stream.by_ref().collect().await;
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events.as_slice(),
            [Err(ProviderError::ResponseParseError { reason })]
                if reason == "SSE stream contained an invalid JSON frame"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn explicit_empty_sse_data_fails_before_a_later_terminal()
    -> Result<(), Box<dyn std::error::Error>> {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        "event: response.created\n\
                         data:\n\n\
                         event: response.completed\n\
                         data: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_late\",\"status\":\"completed\",\"output\":[]}}\n\n",
                    ),
            )
            .mount(&server)
            .await;

        let provider = build_provider(server.uri(), 0);
        let mut stream = provider.stream(build_request())?;
        let events: Vec<_> = stream.by_ref().collect().await;
        assert!(matches!(
            events.as_slice(),
            [Err(ProviderError::ResponseParseError { reason })]
                if reason == "SSE stream contained an invalid JSON frame"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn extra_event_field_space_is_not_normalized_before_validation()
    -> Result<(), Box<dyn std::error::Error>> {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        "event:  response.created\n\
                         data: {\"type\":\"response.created\",\"sequence_number\":0}\n\n",
                    ),
            )
            .mount(&server)
            .await;

        let provider = build_provider(server.uri(), 0);
        let mut stream = provider.stream(build_request())?;
        let events: Vec<_> = stream.by_ref().collect().await;
        assert!(matches!(
            events.as_slice(),
            [Err(ProviderError::ResponseParseError { reason })]
                if reason == "SSE event name does not match Responses payload type"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn mapped_failed_event_yields_exactly_one_terminal_error()
    -> Result<(), Box<dyn std::error::Error>> {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        "event: response.failed\n\
                         data: {\"type\":\"response.failed\",\"sequence_number\":0,\"response\":{\"id\":\"resp_failed\",\"status\":\"failed\",\"output\":[],\"error\":{\"code\":\"invalid_prompt\",\"message\":\"sentinel-private\"}}}\n\n",
                    ),
            )
            .mount(&server)
            .await;

        let provider = build_provider(server.uri(), 0);
        let mut stream = provider.stream(build_request())?;
        let events: Vec<_> = stream.by_ref().collect().await;
        assert!(matches!(
            events.as_slice(),
            [
                Ok(ProviderEvent::ResponseStreamEvent { .. }),
                Err(ProviderError::InvalidRequest { .. })
            ]
        ));
        Ok(())
    }
}
