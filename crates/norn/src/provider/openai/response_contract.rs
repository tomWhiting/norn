//! Pinned Responses API discriminator manifest.
//!
//! This module records the public taxonomy retrieved from the `OpenAI` Developer
//! Docs on 2026-07-16. It deliberately does not dispatch events or parse items:
//! the manifest is the checked boundary that later reconciliation code must
//! exhaustively classify against.
//!
//! The Codex overlay is deliberately scoped to response-stream events, fields,
//! upgrade/metadata headers, and request fields consumed by the pinned Codex
//! Responses transport. It is not an inventory of every Codex HTTP header. In
//! particular, dynamic rate-limit header families and trace-only metadata belong
//! to later transport/accounting phases; the streamed `codex.rate_limits` event
//! is included because it is part of the event dispatcher.

/// Date on which the public schema was retrieved through Developer Docs MCP.
pub const PUBLIC_SCHEMA_RETRIEVED_ON: &str = "2026-07-16";

/// API-description version returned by the official endpoint schema.
pub const PUBLIC_API_DESCRIPTION_VERSION: &str = "2.3.0";

/// Official schema sources used to construct the public registries.
pub const PUBLIC_SCHEMA_SOURCES: [&str; 2] = [
    "https://developers.openai.com/api/reference/resources/responses/streaming-events",
    "https://developers.openai.com/api/reference/resources/responses/methods/create",
];

/// Official guides used to distinguish client-executable from inert items.
pub const PUBLIC_ITEM_SEMANTIC_SOURCES: [&str; 6] = [
    "https://developers.openai.com/api/docs/guides/function-calling",
    "https://developers.openai.com/api/docs/guides/tools-computer-use",
    "https://developers.openai.com/api/docs/guides/tools-shell",
    "https://developers.openai.com/api/docs/guides/tools-apply-patch",
    "https://developers.openai.com/api/docs/guides/tools-connectors-mcp",
    "https://developers.openai.com/api/docs/guides/tools-programmatic-tool-calling",
];

/// Immutable official Codex source revision used for the overlay.
pub const CODEX_SOURCE_COMMIT: &str = "9ff47868eb2afeec579183e01bb9d3d3e9df2bcd";

/// Date on which the Codex source revision was resolved from `main`.
pub const CODEX_SOURCE_RETRIEVED_ON: &str = "2026-07-16";

/// Official Codex source files used to construct the scoped overlay.
pub const CODEX_OVERLAY_SOURCES: [&str; 5] = [
    "https://github.com/openai/codex/blob/9ff47868eb2afeec579183e01bb9d3d3e9df2bcd/codex-rs/codex-api/src/endpoint/responses_websocket.rs",
    "https://github.com/openai/codex/blob/9ff47868eb2afeec579183e01bb9d3d3e9df2bcd/codex-rs/codex-api/src/sse/responses.rs",
    "https://github.com/openai/codex/blob/9ff47868eb2afeec579183e01bb9d3d3e9df2bcd/codex-rs/codex-api/src/rate_limits.rs",
    "https://github.com/openai/codex/blob/9ff47868eb2afeec579183e01bb9d3d3e9df2bcd/codex-rs/codex-api/src/safety_buffering.rs",
    "https://github.com/openai/codex/blob/9ff47868eb2afeec579183e01bb9d3d3e9df2bcd/codex-rs/codex-api/src/common.rs",
];

/// Number of public Responses stream event discriminators in the pinned schema.
pub const PUBLIC_STREAM_EVENT_COUNT: usize = 53;

/// Number of public Responses output-item discriminators in the pinned schema.
pub const PUBLIC_OUTPUT_ITEM_COUNT: usize = 28;

/// Number of Codex overlay surfaces in the pinned P4 stream scope.
pub const CODEX_OVERLAY_COUNT: usize = 18;

/// Number of Codex-only output-item discriminators in the pinned source.
pub const CODEX_OUTPUT_ITEM_COUNT: usize = 0;

/// Processing stage of a public Responses stream event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamEventStage {
    /// A response, item, content part, or hosted operation changed lifecycle state.
    Lifecycle,
    /// The event contributes incremental content to an identity-keyed item.
    Incremental,
    /// A non-terminal component closed; payload authority is event-specific.
    ///
    /// Consumers must not infer reconciliation semantics from this coarse
    /// stage. Some entries carry final content while others are lifecycle-only
    /// closure markers.
    Completed,
    /// The event terminates the response stream or reports a standalone error.
    Terminal,
}

/// One public Responses stream event discriminator and its processing stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamEventEntry {
    name: &'static str,
    stage: StreamEventStage,
}

impl StreamEventEntry {
    const fn new(name: &'static str, stage: StreamEventStage) -> Self {
        Self { name, stage }
    }

    /// Return the exact wire discriminator.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Return the event's processing stage.
    #[must_use]
    pub const fn stage(self) -> StreamEventStage {
        self.stage
    }
}

/// Representation used by Norn's canonical completed-item model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputItemRepresentation {
    /// Norn has a dedicated typed canonical variant for this discriminator.
    TypedCore,
    /// The known public variant is retained losslessly as opaque provider JSON.
    KnownOpaque,
}

/// Whether an output item requires client-side action before continuation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputItemActionability {
    /// The client must execute the request or provide an explicit decision.
    Executable,
    /// The item is provider-owned state, content, or an already-produced output.
    Inert,
}

/// One public Responses output-item discriminator and its handling policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputItemEntry {
    name: &'static str,
    representation: OutputItemRepresentation,
    actionability: OutputItemActionability,
}

impl OutputItemEntry {
    const fn new(
        name: &'static str,
        representation: OutputItemRepresentation,
        actionability: OutputItemActionability,
    ) -> Self {
        Self {
            name,
            representation,
            actionability,
        }
    }

    /// Return the exact wire discriminator.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Return whether the canonical model has a typed variant for this item.
    #[must_use]
    pub const fn representation(self) -> OutputItemRepresentation {
        self.representation
    }

    /// Return whether the item requires client-side action.
    #[must_use]
    pub const fn actionability(self) -> OutputItemActionability {
        self.actionability
    }
}

/// Kind of Codex-only surface layered over the public Responses contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexOverlayKind {
    /// A Codex-only stream event discriminator.
    StreamEvent,
    /// A Codex-only field carried by a public stream event.
    StreamEventField,
    /// A Codex-only request field.
    RequestField,
    /// A Codex-only HTTP response header.
    ResponseHeader,
    /// A field in the Codex WebSocket error-envelope variant.
    WebSocketErrorField,
    /// A Codex-only output-item discriminator.
    OutputItem,
}

/// One Codex-only surface kept outside the public discriminator registries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodexOverlayEntry {
    name: &'static str,
    kind: CodexOverlayKind,
}

impl CodexOverlayEntry {
    const fn new(name: &'static str, kind: CodexOverlayKind) -> Self {
        Self { name, kind }
    }

    /// Return the exact event, header, or dotted field path.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Return the wire surface on which the overlay value appears.
    #[must_use]
    pub const fn kind(self) -> CodexOverlayKind {
        self.kind
    }
}

use OutputItemActionability::{Executable, Inert};
use OutputItemRepresentation::{KnownOpaque, TypedCore};
use StreamEventStage::{Completed, Incremental, Lifecycle, Terminal};

/// Complete pinned public Responses stream event registry.
pub const PUBLIC_STREAM_EVENTS: [StreamEventEntry; PUBLIC_STREAM_EVENT_COUNT] = [
    StreamEventEntry::new("response.created", Lifecycle),
    StreamEventEntry::new("response.in_progress", Lifecycle),
    StreamEventEntry::new("response.completed", Terminal),
    StreamEventEntry::new("response.failed", Terminal),
    StreamEventEntry::new("response.incomplete", Terminal),
    StreamEventEntry::new("response.output_item.added", Lifecycle),
    StreamEventEntry::new("response.output_item.done", Completed),
    StreamEventEntry::new("response.content_part.added", Lifecycle),
    StreamEventEntry::new("response.content_part.done", Completed),
    StreamEventEntry::new("response.output_text.delta", Incremental),
    StreamEventEntry::new("response.output_text.done", Completed),
    StreamEventEntry::new("response.refusal.delta", Incremental),
    StreamEventEntry::new("response.refusal.done", Completed),
    StreamEventEntry::new("response.function_call_arguments.delta", Incremental),
    StreamEventEntry::new("response.function_call_arguments.done", Completed),
    StreamEventEntry::new("response.file_search_call.in_progress", Lifecycle),
    StreamEventEntry::new("response.file_search_call.searching", Lifecycle),
    StreamEventEntry::new("response.file_search_call.completed", Completed),
    StreamEventEntry::new("response.web_search_call.in_progress", Lifecycle),
    StreamEventEntry::new("response.web_search_call.searching", Lifecycle),
    StreamEventEntry::new("response.web_search_call.completed", Completed),
    StreamEventEntry::new("response.reasoning_summary_part.added", Lifecycle),
    StreamEventEntry::new("response.reasoning_summary_part.done", Completed),
    StreamEventEntry::new("response.reasoning_summary_text.delta", Incremental),
    StreamEventEntry::new("response.reasoning_summary_text.done", Completed),
    StreamEventEntry::new("response.reasoning_text.delta", Incremental),
    StreamEventEntry::new("response.reasoning_text.done", Completed),
    StreamEventEntry::new("response.image_generation_call.completed", Completed),
    StreamEventEntry::new("response.image_generation_call.generating", Lifecycle),
    StreamEventEntry::new("response.image_generation_call.in_progress", Lifecycle),
    StreamEventEntry::new("response.image_generation_call.partial_image", Incremental),
    StreamEventEntry::new("response.mcp_call_arguments.delta", Incremental),
    StreamEventEntry::new("response.mcp_call_arguments.done", Completed),
    StreamEventEntry::new("response.mcp_call.completed", Completed),
    StreamEventEntry::new("response.mcp_call.failed", Completed),
    StreamEventEntry::new("response.mcp_call.in_progress", Lifecycle),
    StreamEventEntry::new("response.mcp_list_tools.completed", Completed),
    StreamEventEntry::new("response.mcp_list_tools.failed", Completed),
    StreamEventEntry::new("response.mcp_list_tools.in_progress", Lifecycle),
    StreamEventEntry::new("response.code_interpreter_call.in_progress", Lifecycle),
    StreamEventEntry::new("response.code_interpreter_call.interpreting", Lifecycle),
    StreamEventEntry::new("response.code_interpreter_call.completed", Completed),
    StreamEventEntry::new("response.code_interpreter_call_code.delta", Incremental),
    StreamEventEntry::new("response.code_interpreter_call_code.done", Completed),
    StreamEventEntry::new("response.output_text.annotation.added", Incremental),
    StreamEventEntry::new("response.queued", Lifecycle),
    StreamEventEntry::new("response.custom_tool_call_input.delta", Incremental),
    StreamEventEntry::new("response.custom_tool_call_input.done", Completed),
    StreamEventEntry::new("error", Terminal),
    StreamEventEntry::new("response.audio.delta", Incremental),
    StreamEventEntry::new("response.audio.done", Completed),
    StreamEventEntry::new("response.audio.transcript.delta", Incremental),
    StreamEventEntry::new("response.audio.transcript.done", Completed),
];

/// Complete pinned public Responses output-item registry.
pub const PUBLIC_OUTPUT_ITEMS: [OutputItemEntry; PUBLIC_OUTPUT_ITEM_COUNT] = [
    OutputItemEntry::new("message", TypedCore, Inert),
    OutputItemEntry::new("file_search_call", KnownOpaque, Inert),
    OutputItemEntry::new("function_call", TypedCore, Executable),
    OutputItemEntry::new("function_call_output", KnownOpaque, Inert),
    OutputItemEntry::new("web_search_call", TypedCore, Inert),
    OutputItemEntry::new("computer_call", KnownOpaque, Executable),
    OutputItemEntry::new("computer_call_output", KnownOpaque, Inert),
    OutputItemEntry::new("reasoning", TypedCore, Inert),
    OutputItemEntry::new("program", KnownOpaque, Inert),
    OutputItemEntry::new("program_output", KnownOpaque, Inert),
    OutputItemEntry::new("tool_search_call", KnownOpaque, Inert),
    OutputItemEntry::new("tool_search_output", KnownOpaque, Inert),
    OutputItemEntry::new("additional_tools", KnownOpaque, Inert),
    OutputItemEntry::new("compaction", TypedCore, Inert),
    OutputItemEntry::new("image_generation_call", KnownOpaque, Inert),
    OutputItemEntry::new("code_interpreter_call", KnownOpaque, Inert),
    OutputItemEntry::new("local_shell_call", KnownOpaque, Executable),
    OutputItemEntry::new("local_shell_call_output", KnownOpaque, Inert),
    OutputItemEntry::new("shell_call", KnownOpaque, Executable),
    OutputItemEntry::new("shell_call_output", KnownOpaque, Inert),
    OutputItemEntry::new("apply_patch_call", KnownOpaque, Executable),
    OutputItemEntry::new("apply_patch_call_output", KnownOpaque, Inert),
    OutputItemEntry::new("mcp_call", KnownOpaque, Inert),
    OutputItemEntry::new("mcp_list_tools", KnownOpaque, Inert),
    OutputItemEntry::new("mcp_approval_request", KnownOpaque, Executable),
    OutputItemEntry::new("mcp_approval_response", KnownOpaque, Inert),
    OutputItemEntry::new("custom_tool_call", TypedCore, Executable),
    OutputItemEntry::new("custom_tool_call_output", KnownOpaque, Inert),
];

/// Codex-only wire surfaces pinned separately from the public registries.
pub const CODEX_OVERLAY: [CodexOverlayEntry; CODEX_OVERLAY_COUNT] = [
    CodexOverlayEntry::new("response.metadata", CodexOverlayKind::StreamEvent),
    CodexOverlayEntry::new("codex.rate_limits", CodexOverlayKind::StreamEvent),
    CodexOverlayEntry::new("end_turn", CodexOverlayKind::StreamEventField),
    CodexOverlayEntry::new("safety_buffering", CodexOverlayKind::StreamEventField),
    CodexOverlayEntry::new(
        "response.metadata.metadata.openai_verification_recommendation",
        CodexOverlayKind::StreamEventField,
    ),
    CodexOverlayEntry::new(
        "response.metadata.metadata.openai_chatgpt_moderation_metadata",
        CodexOverlayKind::StreamEventField,
    ),
    CodexOverlayEntry::new("client_metadata", CodexOverlayKind::RequestField),
    CodexOverlayEntry::new("x-codex-turn-state", CodexOverlayKind::ResponseHeader),
    CodexOverlayEntry::new("x-models-etag", CodexOverlayKind::ResponseHeader),
    CodexOverlayEntry::new("x-reasoning-included", CodexOverlayKind::ResponseHeader),
    CodexOverlayEntry::new("openai-model", CodexOverlayKind::ResponseHeader),
    CodexOverlayEntry::new("x-openai-model", CodexOverlayKind::ResponseHeader),
    CodexOverlayEntry::new(
        "x-codex-safety-buffering-enabled",
        CodexOverlayKind::ResponseHeader,
    ),
    CodexOverlayEntry::new(
        "x-codex-safety-buffering-faster-model",
        CodexOverlayKind::ResponseHeader,
    ),
    CodexOverlayEntry::new("error.error", CodexOverlayKind::WebSocketErrorField),
    CodexOverlayEntry::new("error.status", CodexOverlayKind::WebSocketErrorField),
    CodexOverlayEntry::new("error.status_code", CodexOverlayKind::WebSocketErrorField),
    CodexOverlayEntry::new("error.headers", CodexOverlayKind::WebSocketErrorField),
];

/// Look up a public stream-event classification by exact discriminator.
#[must_use]
pub fn public_stream_event(name: &str) -> Option<&'static StreamEventEntry> {
    PUBLIC_STREAM_EVENTS.iter().find(|entry| entry.name == name)
}

/// Look up a public output-item classification by exact discriminator.
#[must_use]
pub fn public_output_item(name: &str) -> Option<&'static OutputItemEntry> {
    PUBLIC_OUTPUT_ITEMS.iter().find(|entry| entry.name == name)
}

/// Look up a Codex overlay surface by exact manifest name.
#[must_use]
pub fn codex_overlay(name: &str) -> Option<&'static CodexOverlayEntry> {
    CODEX_OVERLAY.iter().find(|entry| entry.name == name)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn public_registry_counts_are_pinned() {
        assert_eq!(PUBLIC_STREAM_EVENTS.len(), 53);
        assert_eq!(PUBLIC_OUTPUT_ITEMS.len(), 28);
        assert_eq!(CODEX_OVERLAY.len(), 18);
    }

    #[test]
    fn all_registry_names_are_unique() {
        assert!(unique(
            PUBLIC_STREAM_EVENTS.iter().map(|entry| entry.name())
        ));
        assert!(unique(PUBLIC_OUTPUT_ITEMS.iter().map(|entry| entry.name())));
        assert!(unique(CODEX_OVERLAY.iter().map(|entry| entry.name())));
    }

    #[test]
    fn every_classification_axis_is_represented() {
        for stage in [Lifecycle, Incremental, Completed, Terminal] {
            assert!(
                PUBLIC_STREAM_EVENTS
                    .iter()
                    .any(|entry| entry.stage() == stage)
            );
        }
        for representation in [TypedCore, KnownOpaque] {
            assert!(
                PUBLIC_OUTPUT_ITEMS
                    .iter()
                    .any(|entry| entry.representation() == representation)
            );
        }
        for actionability in [Executable, Inert] {
            assert!(
                PUBLIC_OUTPUT_ITEMS
                    .iter()
                    .any(|entry| entry.actionability() == actionability)
            );
        }
    }

    #[test]
    fn public_and_codex_registries_do_not_overlap() {
        let public: BTreeSet<_> = PUBLIC_STREAM_EVENTS
            .iter()
            .map(|entry| entry.name())
            .chain(PUBLIC_OUTPUT_ITEMS.iter().map(|entry| entry.name()))
            .collect();
        assert!(
            CODEX_OVERLAY
                .iter()
                .all(|entry| !public.contains(entry.name()))
        );
    }

    #[test]
    fn codex_output_item_overlay_is_explicitly_empty() {
        let count = CODEX_OVERLAY
            .iter()
            .filter(|entry| entry.kind() == CodexOverlayKind::OutputItem)
            .count();
        assert_eq!(count, CODEX_OUTPUT_ITEM_COUNT);
    }

    #[test]
    fn typed_core_and_executable_sets_are_explicit() {
        assert_eq!(
            names_with_representation(TypedCore),
            [
                "message",
                "function_call",
                "web_search_call",
                "reasoning",
                "compaction",
                "custom_tool_call",
            ]
        );
        assert_eq!(
            names_with_actionability(Executable),
            [
                "function_call",
                "computer_call",
                "local_shell_call",
                "shell_call",
                "apply_patch_call",
                "mcp_approval_request",
                "custom_tool_call",
            ]
        );
    }

    fn unique<'a>(mut names: impl Iterator<Item = &'a str>) -> bool {
        let mut seen = BTreeSet::new();
        names.all(|name| seen.insert(name))
    }

    fn names_with_representation(representation: OutputItemRepresentation) -> Vec<&'static str> {
        PUBLIC_OUTPUT_ITEMS
            .iter()
            .filter(|entry| entry.representation() == representation)
            .map(|entry| entry.name())
            .collect()
    }

    fn names_with_actionability(actionability: OutputItemActionability) -> Vec<&'static str> {
        PUBLIC_OUTPUT_ITEMS
            .iter()
            .filter(|entry| entry.actionability() == actionability)
            .map(|entry| entry.name())
            .collect()
    }
}
