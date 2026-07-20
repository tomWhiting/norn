//! Streaming event types emitted by providers.

use super::openai::response_stream_event::ResponseStreamEvent;
use super::reasoning::ReasoningItem;
use super::request::ToolCallKind;
use super::response_audio::ResponseAudioEvent;
use super::response_item::ResponseTranscriptItem;
use super::usage::Usage;
use crate::error::ProviderError;

/// Reason the model stopped generating.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The model finished its turn naturally.
    EndTurn,
    /// The backend requested another response within the same user turn.
    ///
    /// The ChatGPT/Codex Responses overlay emits this when
    /// `response.completed.response.end_turn` is explicitly `false`.
    ContinueTurn,
    /// The model wants to call one or more tools.
    ToolUse,
    /// The model hit the maximum token limit.
    MaxTokens,
    /// The model's output was filtered by a content policy.
    ContentFilter,
}

/// A single streaming event from a provider response.
///
/// Each variant carries only delta data, not accumulated state.
#[derive(Clone, Debug)]
pub enum ProviderEvent {
    /// One validated `OpenAI` Responses stream envelope.
    ///
    /// This is the observability lane for the complete public/Codex event
    /// taxonomy. It never becomes replayable conversation state by itself;
    /// authoritative completed items arrive separately through
    /// [`ResponseItemDone`](Self::ResponseItemDone). Provider JSON is retained
    /// exactly except that reusable transport credentials such as
    /// `x-codex-turn-state` are redacted before disclosure.
    ResponseStreamEvent {
        /// Validated envelope retaining non-credential provider JSON exactly.
        event: Box<ResponseStreamEvent>,
    },

    /// One validated response-scoped audio or audio-transcript frame.
    ///
    /// The lossless [`ResponseStreamEvent`](Self::ResponseStreamEvent) is
    /// emitted first. This typed projection is emitted only after response-level
    /// lifecycle validation accepts the frame; exact duplicate sequences remain
    /// raw-observable but do not produce a second actionable audio frame. The
    /// source envelope is repeated here so persistence does not depend on event
    /// adjacency in downstream queues.
    ResponseAudioFrame {
        /// Exact validated envelope from which the typed event was decoded.
        stream_event: Box<ResponseStreamEvent>,
        /// Typed event carrying decoded bytes or transcript text.
        event: ResponseAudioEvent,
    },

    /// A chunk of text content from the model.
    TextDelta {
        /// The text fragment.
        text: String,
    },

    /// A refusal-content preview from the model.
    ///
    /// Refusal is a model outcome, not a transport failure. Identity and
    /// indices remain attached so interleaved message parts cannot collapse
    /// into one unkeyed string.
    RefusalDelta {
        /// Provider output-item identifier.
        item_id: String,
        /// Position of the message in `response.output`.
        output_index: u64,
        /// Position of the refusal part in the message content array.
        content_index: u64,
        /// Incremental refusal text.
        refusal: String,
    },

    /// Authoritative refusal text from `response.refusal.done`.
    RefusalComplete {
        /// Provider output-item identifier.
        item_id: String,
        /// Position of the message in `response.output`.
        output_index: u64,
        /// Position of the refusal part in the message content array.
        content_index: u64,
        /// Complete refusal text.
        refusal: String,
    },

    /// A chunk of thinking/reasoning content.
    ThinkingDelta {
        /// The reasoning text fragment.
        text: String,
    },

    /// A partial tool call being assembled.
    ToolCallDelta {
        /// Streaming item identifier used to merge deltas of the same call
        /// (the `fc_*` `item_id` on the wire). This is NOT the `call_id` the
        /// model expects on `function_call_output` echoes — that arrives on
        /// [`ProviderEvent::ToolCallComplete`] and, for correlation, on the
        /// `call_id` field below.
        item_id: String,
        /// Provider-assigned correlation identifier (`call_*` on the `OpenAI`
        /// Responses wire) for the tool call these deltas belong to, when the
        /// provider has surfaced it by the time this fragment is emitted.
        ///
        /// This is the same `call_id` that arrives on
        /// [`ProviderEvent::ToolCallComplete`] and echoes on
        /// `function_call_output`; carrying it here lets an embedder correlate
        /// live input-streaming deltas with the tool call its UI already knows
        /// (by `call_id`) rather than the internal streaming `item_id`.
        ///
        /// Source and guarantee, by provider family:
        ///
        /// * **`OpenAI` Responses API** — populated from the
        ///   `response.output_item.added` event, which announces the item
        ///   carrying both its `item_id` and its `call_id` and *always*
        ///   precedes that item's `response.function_call_arguments.delta` /
        ///   `response.custom_tool_call_input.delta` events within a response.
        ///   It is therefore always `Some` on this path (see the
        ///   `ResponsesMapper` correlation logic).
        /// * **Chat Completions** — the tool-call `id` from the first streaming
        ///   chunk of the call; `Some` from that chunk onward.
        /// * **Anthropic** — the tool `id` on the tool-use block when it is
        ///   available in the emitting event; `None` for the incremental
        ///   `input_json_delta` fragments, which the wire delivers without the
        ///   id in the same event.
        ///
        /// `None` means the provider had not surfaced the correlation id at the
        /// time this fragment was produced — it is never fabricated.
        call_id: Option<String>,
        /// Tool name (present in the first delta for this call).
        name: Option<String>,
        /// Incremental arguments fragment. For
        /// [`ToolCallKind::Function`]
        /// deltas this is partial JSON; for
        /// [`ToolCallKind::Custom`]
        /// deltas this is a freeform `input` fragment.
        arguments_delta: String,
        /// Which surface kind this delta belongs to. Derived from the SSE
        /// event type (`response.function_call_arguments.delta` vs
        /// `response.custom_tool_call_input.delta`).
        kind: ToolCallKind,
    },

    /// Complete text content from a `.done` SSE event.
    TextComplete {
        /// The full accumulated text.
        text: String,
    },

    /// Complete thinking/reasoning content from a `.done` SSE event.
    ThinkingComplete {
        /// The full accumulated reasoning text.
        text: String,
    },

    /// A complete reasoning output item from a `response.output_item.done`
    /// SSE event (`item.type == "reasoning"`).
    ///
    /// Distinct from [`ThinkingDelta`](Self::ThinkingDelta) /
    /// [`ThinkingComplete`](Self::ThinkingComplete), which carry only the
    /// display text: this event carries the full structured item —
    /// including `encrypted_content` — that response assembly attaches to
    /// the assistant [`Message`](crate::provider::request::Message) so the
    /// Responses API serializer can replay it on stateless backends.
    ReasoningItemDone {
        /// The captured reasoning item.
        item: ReasoningItem,
    },

    /// One authoritative completed Responses output item.
    ///
    /// The replayable provider JSON remains on the item; stream coordinates
    /// are retained separately for identity-keyed reconciliation.
    ResponseItemDone {
        /// Lossless item plus non-replayable stream provenance.
        item: ResponseTranscriptItem,
    },

    /// A fully assembled tool call from an `output_item.done` SSE event.
    ToolCallComplete {
        /// Provider-assigned correlation identifier (the `call_*` `call_id`
        /// on the wire). This is the only identifier the model accepts on a
        /// subsequent `function_call_output` echo — the streaming `item_id`
        /// MUST NOT be substituted here.
        call_id: String,
        /// Tool name.
        name: String,
        /// Complete arguments (`function_call`) or freeform input
        /// (`custom_tool_call`) string, disambiguated by `kind`.
        arguments: String,
        /// Which surface kind this completion is for.
        kind: ToolCallKind,
    },

    /// A tool execution result, broadcast after tool dispatch completes.
    ToolResult {
        /// The tool call this result is for.
        tool_call_id: String,
        /// Name of the tool that was executed.
        tool_name: String,
        /// Serialized output from the tool.
        output: serde_json::Value,
        /// Wall-clock execution time in milliseconds.
        duration_ms: u64,
    },

    /// Opaque provider-side compaction item.
    Compaction {
        /// Provider item type that carried the compaction payload.
        item_type: String,
        /// Encrypted provider payload, when present.
        encrypted_content: Option<String>,
    },

    /// The provider finished this response.
    Done {
        /// Why the model stopped.
        stop_reason: StopReason,
        /// Token usage for this call.
        usage: Usage,
        /// Server-assigned response ID for conversation chaining.
        response_id: Option<String>,
    },

    /// The provider reported an error during streaming.
    Error {
        /// The error details.
        error: ProviderError,
    },
}
