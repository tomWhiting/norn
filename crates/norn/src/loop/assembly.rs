//! Response assembly from provider event streams.

use std::collections::HashMap;

use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::reasoning::ReasoningItem;
use crate::provider::request::ToolCallKind;
use crate::provider::usage::Usage;

/// A tool call accumulated from streaming deltas.
#[derive(Clone, Debug)]
pub struct AssembledToolCall {
    /// Provider-assigned correlation identifier (`call_*`). This is the
    /// only identifier the model accepts on a follow-up
    /// `function_call_output` echo. When a `ToolCallComplete` event arrives
    /// during streaming, its `call_id` populates this field; otherwise the
    /// streaming merge key falls through with a warning and the loop will
    /// likely see the model reject the echo.
    pub call_id: String,
    /// Tool name (captured from first delta with a name).
    pub name: String,
    /// Complete arguments string (concatenated from deltas).
    pub arguments: String,
    /// Which surface kind this call uses. Defaults to
    /// [`ToolCallKind::Function`]; promoted to
    /// [`ToolCallKind::Custom`](crate::provider::request::ToolCallKind::Custom)
    /// when the originating SSE events are `response.custom_tool_call_input.*`
    /// or `response.output_item.done` carries `item.type == "custom_tool_call"`.
    pub kind: ToolCallKind,
}

/// A fully assembled response from one provider turn.
#[derive(Clone, Debug)]
pub struct AssembledResponse {
    /// Accumulated text content.
    pub text: String,
    /// Accumulated reasoning/thinking content.
    pub thinking: String,
    /// Structured reasoning output items, in wire order. Attached to the
    /// assistant [`Message`](crate::provider::request::Message) so the
    /// `OpenAI` Responses serializer can replay encrypted reasoning on
    /// stateless backends.
    pub reasoning: Vec<ReasoningItem>,
    /// Accumulated tool calls.
    pub tool_calls: Vec<AssembledToolCall>,
    /// Stop reason from the Done event.
    pub stop_reason: StopReason,
    /// Token usage from the Done event.
    pub usage: Usage,
    /// Server-assigned response ID for conversation chaining.
    pub response_id: Option<String>,
}

/// In-progress assembly slot for one tool call.
///
/// Keyed by the streaming merge key (`item_id` on the wire — `fc_*`).
/// `call_id` is `None` until a [`ProviderEvent::ToolCallComplete`] arrives
/// and attaches the correlation identifier (`call_*`) the model echoes.
struct ToolCallAccumulator {
    name: String,
    arguments: String,
    call_id: Option<String>,
    /// Surface kind, derived from the first delta or
    /// [`ProviderEvent::ToolCallComplete`] event for this slot. A later
    /// `ToolCallComplete` overrides the delta-derived value so the wire
    /// classification on `output_item.done` wins if the two ever disagree.
    kind: ToolCallKind,
}

/// Assembles a complete response from a sequence of `ProviderEvent` values.
///
/// Text deltas are concatenated. Tool call deltas are grouped by their
/// streaming merge key (`item_id`) and concatenated. When a `ToolCallComplete`
/// event arrives it attaches its `call_id` (and may supersede `name`/
/// `arguments` if the deltas were empty) to the first pending entry without
/// a `call_id`, preserving the wire-protocol order guarantee. If only
/// deltas arrive for an item, the merge key is promoted to `call_id` as a
/// fallback with a warning — the model will likely reject the echo, but
/// emitting the call is still safer than dropping it.
///
/// # Ordering invariant
///
/// This function relies on every `ToolCallComplete` corresponding to *exactly
/// the first pending entry* — the merge key whose accumulator has not yet
/// received a `call_id`. That invariant holds only because the `OpenAI` request
/// is dispatched with `parallel_tool_calls: false` (see
/// [`build_payload`](crate::provider::openai::request) — emitting
/// `"parallel_tool_calls": false` at `request.rs:106`). The Responses API
/// then serialises tool calls so each `response.output_item.done` event for a
/// `function_call` (or `custom_tool_call`) arrives in the same order the
/// model emitted the calls, and the deltas for the *next* call do not
/// interleave with the previous call's `done`. If parallel tool calls are
/// ever enabled, `ToolCallComplete` would need to carry the same `item_id` as
/// its deltas (currently it does not) so the attach step can target the
/// matching accumulator directly rather than relying on order.
///
/// Classification happens in the caller after assembly.
pub fn assemble_response(events: &[ProviderEvent]) -> Option<AssembledResponse> {
    let mut text = String::new();
    let mut thinking = String::new();
    let mut reasoning: Vec<ReasoningItem> = Vec::new();
    let mut tool_calls_map: HashMap<String, ToolCallAccumulator> = HashMap::new();
    let mut tool_call_order: Vec<String> = Vec::new();
    let mut stop_reason = StopReason::EndTurn;
    let mut usage = Usage::default();
    let mut response_id = None;
    let mut saw_done = false;

    for event in events {
        match event {
            ProviderEvent::TextDelta { text: delta } => {
                text.push_str(delta);
            }
            ProviderEvent::ToolCallDelta {
                item_id,
                // `call_id` correlates live deltas for embedders; assembly
                // merges by `item_id` and takes the authoritative `call_id`
                // from the eventual `ToolCallComplete`.
                call_id: _,
                name,
                arguments_delta,
                kind,
            } => {
                let entry = tool_calls_map.entry(item_id.clone()).or_insert_with(|| {
                    tool_call_order.push(item_id.clone());
                    ToolCallAccumulator {
                        name: String::new(),
                        arguments: String::new(),
                        call_id: None,
                        kind: *kind,
                    }
                });
                if let Some(n) = name {
                    entry.name.clone_from(n);
                }
                entry.arguments.push_str(arguments_delta);
                // A later delta on the same merge key with a different kind
                // would indicate a wire-protocol inconsistency; trust the
                // most-recent classification rather than the first one.
                entry.kind = *kind;
            }
            ProviderEvent::Done {
                stop_reason: sr,
                usage: u,
                response_id: rid,
            } => {
                stop_reason = sr.clone();
                usage = u.clone();
                response_id.clone_from(rid);
                saw_done = true;
            }
            ProviderEvent::ToolCallComplete {
                call_id,
                name,
                arguments,
                kind,
            } => {
                // The wire protocol guarantees output_item.done events arrive
                // in the same order their item_id deltas were streamed, so
                // the first pending entry (first merge key without an
                // attached call_id) is the one this Complete corresponds to.
                let attach_key = tool_call_order.iter().find(|key| {
                    tool_calls_map
                        .get(key.as_str())
                        .is_some_and(|entry| entry.call_id.is_none())
                });
                if let Some(key) = attach_key.cloned() {
                    if let Some(entry) = tool_calls_map.get_mut(&key) {
                        if !name.is_empty() {
                            entry.name.clone_from(name);
                        }
                        if entry.arguments.is_empty() && !arguments.is_empty() {
                            entry.arguments.clone_from(arguments);
                        }
                        entry.call_id = Some(call_id.clone());
                        // The `output_item.done` classification is the
                        // authoritative kind for this call — override any
                        // earlier delta-derived value.
                        entry.kind = *kind;
                    }
                } else {
                    // Complete with no preceding deltas — create the entry
                    // outright using call_id as both the map key and the
                    // final correlation identifier.
                    tool_call_order.push(call_id.clone());
                    tool_calls_map.insert(
                        call_id.clone(),
                        ToolCallAccumulator {
                            name: name.clone(),
                            arguments: arguments.clone(),
                            call_id: Some(call_id.clone()),
                            kind: *kind,
                        },
                    );
                }
            }
            ProviderEvent::ThinkingDelta { text: delta } => {
                thinking.push_str(delta);
            }
            ProviderEvent::ReasoningItemDone { item } => {
                reasoning.push(item.clone());
            }
            // None of these carry assemblable content. `Error` is
            // additionally unreachable through the loop: `call_provider`
            // fails the turn with the event's typed `ProviderError`
            // before assembly runs.
            ProviderEvent::TextComplete { .. }
            | ProviderEvent::ThinkingComplete { .. }
            | ProviderEvent::ToolResult { .. }
            | ProviderEvent::Compaction { .. }
            | ProviderEvent::Error { .. } => {}
        }
    }

    if !saw_done {
        return None;
    }

    let tool_calls: Vec<AssembledToolCall> = tool_call_order
        .iter()
        .filter_map(|merge_key| {
            let entry = tool_calls_map.get(merge_key)?;
            let call_id = entry.call_id.clone().unwrap_or_else(|| {
                tracing::warn!(
                    "ToolCallComplete never arrived; promoting streaming merge key to call_id (the model will likely reject the echo)",
                );
                merge_key.clone()
            });
            Some(AssembledToolCall {
                call_id,
                name: entry.name.clone(),
                arguments: entry.arguments.clone(),
                kind: entry.kind,
            })
        })
        .collect();

    Some(AssembledResponse {
        text,
        thinking,
        reasoning,
        tool_calls,
        stop_reason,
        usage,
        response_id,
    })
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
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct SharedLog(Arc<Mutex<Vec<u8>>>);

    impl io::Write for SharedLog {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let mut destination = self
                .0
                .lock()
                .map_err(|error| io::Error::other(format!("shared log lock poisoned: {error}")))?;
            std::io::Write::write(&mut *destination, buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for SharedLog {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    impl SharedLog {
        fn rendered(&self) -> io::Result<String> {
            let bytes = self
                .0
                .lock()
                .map_err(|error| io::Error::other(format!("shared log lock poisoned: {error}")))?
                .clone();
            String::from_utf8(bytes).map_err(io::Error::other)
        }
    }

    #[test]
    fn text_only_response() {
        let events = vec![
            ProviderEvent::TextDelta {
                text: "hello ".to_string(),
            },
            ProviderEvent::TextDelta {
                text: "world".to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.text, "hello world");
        assert!(resp.tool_calls.is_empty());
    }

    #[test]
    fn text_and_tool_call() {
        let events = vec![
            ProviderEvent::TextDelta {
                text: "x".to_string(),
            },
            ProviderEvent::ToolCallDelta {
                item_id: "1".to_string(),
                call_id: None,
                name: Some("read".to_string()),
                arguments_delta: "{\"path\":".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::ToolCallDelta {
                item_id: "1".to_string(),
                call_id: None,
                name: None,
                arguments_delta: "\"f\"}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.text, "x");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "read");
        assert_eq!(resp.tool_calls[0].arguments, "{\"path\":\"f\"}");
    }

    #[test]
    fn multiple_tool_calls() {
        let events = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "1".to_string(),
                call_id: None,
                name: Some("read".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::ToolCallDelta {
                item_id: "2".to_string(),
                call_id: None,
                name: Some("write".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.tool_calls.len(), 2);
        // Deltas with no Complete fall back to the merge key as call_id.
        assert_eq!(resp.tool_calls[0].call_id, "1");
        assert_eq!(resp.tool_calls[1].call_id, "2");
    }

    #[test]
    fn no_done_returns_none() {
        let events = vec![ProviderEvent::TextDelta {
            text: "partial".to_string(),
        }];
        assert!(assemble_response(&events).is_none());
    }

    #[test]
    fn classification_not_during_streaming() {
        let events = vec![
            ProviderEvent::TextDelta {
                text: "text".to_string(),
            },
            ProviderEvent::ToolCallDelta {
                item_id: "1".to_string(),
                call_id: None,
                name: Some("tool".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.text, "text");
        assert_eq!(resp.tool_calls.len(), 1);
    }

    #[test]
    fn tool_call_complete_delivers_name_and_arguments() {
        let events = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc_1".to_string(),
                call_id: None,
                name: None,
                arguments_delta: "{\"city\":\"NYC\"}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::ToolCallComplete {
                call_id: "call_xyz".to_string(),
                name: "get_weather".to_string(),
                arguments: "{\"city\":\"NYC\"}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "get_weather");
        assert_eq!(resp.tool_calls[0].arguments, "{\"city\":\"NYC\"}");
    }

    #[test]
    fn thinking_only_response() {
        let events = vec![
            ProviderEvent::ThinkingDelta {
                text: "let me ".to_string(),
            },
            ProviderEvent::ThinkingDelta {
                text: "reason".to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.thinking, "let me reason");
        assert!(resp.text.is_empty());
        assert!(resp.tool_calls.is_empty());
    }

    #[test]
    fn mixed_thinking_text_and_tool() {
        let events = vec![
            ProviderEvent::ThinkingDelta {
                text: "first I think".to_string(),
            },
            ProviderEvent::TextDelta {
                text: "hello".to_string(),
            },
            ProviderEvent::ToolCallDelta {
                item_id: "1".to_string(),
                call_id: None,
                name: Some("read".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.thinking, "first I think");
        assert_eq!(resp.text, "hello");
        assert_eq!(resp.tool_calls.len(), 1);
    }

    #[test]
    fn text_only_response_has_empty_thinking() {
        let events = vec![
            ProviderEvent::TextDelta {
                text: "hi".to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert!(resp.thinking.is_empty());
        assert_eq!(resp.text, "hi");
    }

    #[test]
    fn multiple_tool_calls_attach_call_ids_in_order() {
        // F4: locks in the ordering invariant that depends on
        // `parallel_tool_calls: false`. With two pending merge keys (`fc_a`,
        // `fc_b`) and two `ToolCallComplete` events arriving in order, the
        // first Complete must attach to `fc_a` and the second to `fc_b`.
        // Swapping the order of the Completes would prove the invariant fails
        // if parallel tool calls are ever turned on without reworking
        // attachment to use `item_id`.
        let events = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "fc_a".to_string(),
                call_id: None,
                name: Some("read".to_string()),
                arguments_delta: "{\"path\":\"a\"}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::ToolCallDelta {
                item_id: "fc_b".to_string(),
                call_id: None,
                name: Some("write".to_string()),
                arguments_delta: "{\"path\":\"b\"}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::ToolCallComplete {
                call_id: "call_alpha".to_string(),
                name: "read".to_string(),
                arguments: "{\"path\":\"a\"}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::ToolCallComplete {
                call_id: "call_beta".to_string(),
                name: "write".to_string(),
                arguments: "{\"path\":\"b\"}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.tool_calls.len(), 2);
        // Iteration order preserved from `tool_call_order`: fc_a first.
        assert_eq!(resp.tool_calls[0].name, "read");
        assert_eq!(
            resp.tool_calls[0].call_id, "call_alpha",
            "first Complete must bind to first merge key (fc_a)",
        );
        assert_eq!(resp.tool_calls[1].name, "write");
        assert_eq!(
            resp.tool_calls[1].call_id, "call_beta",
            "second Complete must bind to second merge key (fc_b)",
        );
    }

    #[test]
    fn custom_tool_call_deltas_then_complete_preserve_custom_kind() {
        // F5: streaming a custom tool call end-to-end — deltas carry
        // ToolCallKind::Custom, the Complete reconfirms it, and the
        // assembled call must surface as Custom for the serializer.
        let events = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "ctc_1".to_string(),
                call_id: None,
                name: Some("apply_patch".to_string()),
                arguments_delta: "*** BEGIN ".to_string(),
                kind: ToolCallKind::Custom,
            },
            ProviderEvent::ToolCallDelta {
                item_id: "ctc_1".to_string(),
                call_id: None,
                name: None,
                arguments_delta: "PATCH ***".to_string(),
                kind: ToolCallKind::Custom,
            },
            ProviderEvent::ToolCallComplete {
                call_id: "call_custom".to_string(),
                name: "apply_patch".to_string(),
                arguments: "*** BEGIN PATCH ***".to_string(),
                kind: ToolCallKind::Custom,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].call_id, "call_custom");
        assert_eq!(resp.tool_calls[0].name, "apply_patch");
        assert_eq!(resp.tool_calls[0].arguments, "*** BEGIN PATCH ***");
        assert_eq!(resp.tool_calls[0].kind, ToolCallKind::Custom);
    }

    #[test]
    fn function_call_default_kind_when_only_deltas() {
        // The legacy delta-only fallback path (no Complete arrived) inherits
        // the kind from the delta event, not a hardcoded default. A function
        // delta must therefore yield a function-kind assembled call.
        let events = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "fc_1".to_string(),
                call_id: None,
                name: Some("read".to_string()),
                arguments_delta: "{}".to_string(),
                kind: ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].kind, ToolCallKind::Function);
    }

    #[test]
    fn fallback_warning_does_not_disclose_streaming_merge_key()
    -> Result<(), Box<dyn std::error::Error>> {
        const SECRET_MERGE_KEY: &str = "merge-key-secret-must-not-escape";
        let events = vec![
            ProviderEvent::ToolCallDelta {
                item_id: SECRET_MERGE_KEY.to_owned(),
                call_id: None,
                name: Some("read".to_owned()),
                arguments_delta: "{}".to_owned(),
                kind: ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let logs = SharedLog::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(logs.clone())
            .finish();
        let response = tracing::subscriber::with_default(subscriber, || assemble_response(&events))
            .ok_or_else(|| io::Error::other("delta-only response did not assemble"))?;

        assert_eq!(response.tool_calls[0].call_id, SECRET_MERGE_KEY);
        let rendered = logs.rendered()?;
        assert!(
            rendered.contains("ToolCallComplete never arrived"),
            "warning was not captured: {rendered}"
        );
        assert!(!rendered.contains(SECRET_MERGE_KEY), "trace: {rendered}");
        Ok(())
    }

    #[test]
    fn complete_overrides_delta_kind_when_they_disagree() {
        // If a delta arrived with one kind and the matching Complete carries
        // another, the Complete (the wire's `output_item.done`) wins. The
        // server's classification on done is authoritative.
        let events = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "x_1".to_string(),
                call_id: None,
                name: None,
                arguments_delta: "hello".to_string(),
                kind: ToolCallKind::Function,
            },
            ProviderEvent::ToolCallComplete {
                call_id: "call_x".to_string(),
                name: "freeform".to_string(),
                arguments: "hello".to_string(),
                kind: ToolCallKind::Custom,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.tool_calls[0].kind, ToolCallKind::Custom);
    }

    #[test]
    fn reasoning_items_collected_in_wire_order() {
        // Encrypted-reasoning seam: ReasoningItemDone events are attached
        // to the assembled response in the order the wire emitted them, so
        // the request serializer can replay them ahead of the assistant
        // output they preceded.
        use crate::provider::reasoning::ReasoningSummaryPart;
        let first = ReasoningItem {
            id: "rs_1".to_owned(),
            summary: vec![ReasoningSummaryPart::SummaryText {
                text: "first thought".to_owned(),
            }],
            content: None,
            encrypted_content: Some("blob-1".to_owned()),
        };
        let second = ReasoningItem {
            id: "rs_2".to_owned(),
            summary: Vec::new(),
            content: None,
            encrypted_content: None,
        };
        let events = vec![
            ProviderEvent::ReasoningItemDone {
                item: first.clone(),
            },
            ProviderEvent::TextDelta {
                text: "answer".to_string(),
            },
            ProviderEvent::ReasoningItemDone {
                item: second.clone(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.text, "answer");
        assert_eq!(resp.reasoning, vec![first, second]);
    }

    #[test]
    fn response_without_reasoning_items_has_empty_reasoning() {
        let events = vec![
            ProviderEvent::TextDelta {
                text: "hi".to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert!(resp.reasoning.is_empty());
    }

    #[test]
    fn tool_call_complete_without_prior_deltas() {
        let events = vec![
            ProviderEvent::ToolCallComplete {
                call_id: "call_bash".to_string(),
                name: "bash".to_string(),
                arguments: "{\"cmd\":\"ls\"}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let resp = assemble_response(&events).expect("assembled");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "bash");
        assert_eq!(resp.tool_calls[0].arguments, "{\"cmd\":\"ls\"}");
    }
}
