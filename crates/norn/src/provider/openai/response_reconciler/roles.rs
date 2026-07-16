//! Exhaustive reconciliation roles for the pinned public event taxonomy.

use super::{
    ReconcileUpdate, ResponseDeltaChannel, ResponseReconciler, ResponseReconciliationError,
};
use crate::provider::openai::sse::SseEvent;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum ItemStringKind {
    McpArguments,
    CodeInterpreterCode,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum HostedFamily {
    FileSearch,
    WebSearch,
    ImageGeneration,
    McpCall,
    McpListTools,
    CodeInterpreter,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum HostedPhase {
    InProgress,
    Active,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResponseEventRole {
    RawPreviewOnly,
    TerminalAuthority,
    ProviderError,
    ItemAdded,
    ItemDone,
    CoreStringDelta,
    CoreStringDone,
    ContentPartAdded,
    ContentPartDone,
    ReasoningSummaryPartAdded,
    ReasoningSummaryPartDone,
    AnnotationAdded,
    ImagePartial,
    ItemStringDelta(ItemStringKind),
    ItemStringDone(ItemStringKind),
    HostedLifecycle(HostedFamily, HostedPhase),
    UnsupportedMedia,
}

pub(super) fn response_event_role(event_type: &str) -> Option<ResponseEventRole> {
    use HostedFamily::{CodeInterpreter, ImageGeneration, McpCall, McpListTools};
    use HostedPhase::{Active, Completed, Failed, InProgress};
    use ItemStringKind::{CodeInterpreterCode, McpArguments};
    use ResponseEventRole::{
        AnnotationAdded, ContentPartAdded, ContentPartDone, CoreStringDelta, CoreStringDone,
        HostedLifecycle, ImagePartial, ItemAdded, ItemDone, ItemStringDelta, ItemStringDone,
        ProviderError, RawPreviewOnly, ReasoningSummaryPartAdded, ReasoningSummaryPartDone,
        TerminalAuthority, UnsupportedMedia,
    };

    match event_type {
        "response.created" | "response.in_progress" | "response.queued" => Some(RawPreviewOnly),
        "response.completed" | "response.failed" | "response.incomplete" => Some(TerminalAuthority),
        "response.output_item.added" => Some(ItemAdded),
        "response.output_item.done" => Some(ItemDone),
        "response.content_part.added" => Some(ContentPartAdded),
        "response.content_part.done" => Some(ContentPartDone),
        "response.reasoning_summary_part.added" => Some(ReasoningSummaryPartAdded),
        "response.reasoning_summary_part.done" => Some(ReasoningSummaryPartDone),
        "response.output_text.delta"
        | "response.refusal.delta"
        | "response.function_call_arguments.delta"
        | "response.reasoning_summary_text.delta"
        | "response.reasoning_text.delta"
        | "response.custom_tool_call_input.delta" => Some(CoreStringDelta),
        "response.output_text.done"
        | "response.refusal.done"
        | "response.function_call_arguments.done"
        | "response.reasoning_summary_text.done"
        | "response.reasoning_text.done"
        | "response.custom_tool_call_input.done" => Some(CoreStringDone),
        "response.file_search_call.in_progress" => {
            Some(HostedLifecycle(HostedFamily::FileSearch, InProgress))
        }
        "response.file_search_call.searching" => {
            Some(HostedLifecycle(HostedFamily::FileSearch, Active))
        }
        "response.file_search_call.completed" => {
            Some(HostedLifecycle(HostedFamily::FileSearch, Completed))
        }
        "response.web_search_call.in_progress" => {
            Some(HostedLifecycle(HostedFamily::WebSearch, InProgress))
        }
        "response.web_search_call.searching" => {
            Some(HostedLifecycle(HostedFamily::WebSearch, Active))
        }
        "response.web_search_call.completed" => {
            Some(HostedLifecycle(HostedFamily::WebSearch, Completed))
        }
        "response.image_generation_call.in_progress" => {
            Some(HostedLifecycle(ImageGeneration, InProgress))
        }
        "response.image_generation_call.generating" => {
            Some(HostedLifecycle(ImageGeneration, Active))
        }
        "response.image_generation_call.completed" => {
            Some(HostedLifecycle(ImageGeneration, Completed))
        }
        "response.image_generation_call.partial_image" => Some(ImagePartial),
        "response.mcp_call_arguments.delta" => Some(ItemStringDelta(McpArguments)),
        "response.mcp_call_arguments.done" => Some(ItemStringDone(McpArguments)),
        "response.mcp_call.in_progress" => Some(HostedLifecycle(McpCall, InProgress)),
        "response.mcp_call.completed" => Some(HostedLifecycle(McpCall, Completed)),
        "response.mcp_call.failed" => Some(HostedLifecycle(McpCall, Failed)),
        "response.mcp_list_tools.in_progress" => Some(HostedLifecycle(McpListTools, InProgress)),
        "response.mcp_list_tools.completed" => Some(HostedLifecycle(McpListTools, Completed)),
        "response.mcp_list_tools.failed" => Some(HostedLifecycle(McpListTools, Failed)),
        "response.code_interpreter_call.in_progress" => {
            Some(HostedLifecycle(CodeInterpreter, InProgress))
        }
        "response.code_interpreter_call.interpreting" => {
            Some(HostedLifecycle(CodeInterpreter, Active))
        }
        "response.code_interpreter_call.completed" => {
            Some(HostedLifecycle(CodeInterpreter, Completed))
        }
        "response.code_interpreter_call_code.delta" => Some(ItemStringDelta(CodeInterpreterCode)),
        "response.code_interpreter_call_code.done" => Some(ItemStringDone(CodeInterpreterCode)),
        "response.output_text.annotation.added" => Some(AnnotationAdded),
        "response.audio.delta"
        | "response.audio.done"
        | "response.audio.transcript.delta"
        | "response.audio.transcript.done" => Some(UnsupportedMedia),
        "error" => Some(ProviderError),
        _ => None,
    }
}

impl ResponseReconciler {
    pub(super) fn apply(
        &mut self,
        event: &SseEvent,
        sequence_number: u64,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let role = response_event_role(&event.event_type)
            .ok_or(ResponseReconciliationError::UnclassifiedPublicEvent)?;
        match role {
            ResponseEventRole::RawPreviewOnly | ResponseEventRole::ProviderError => {
                Ok(ReconcileUpdate::Ignored)
            }
            ResponseEventRole::TerminalAuthority => self.finish(event, sequence_number),
            ResponseEventRole::ItemAdded => self.add_item(event),
            ResponseEventRole::ItemDone => self.complete_item(event, sequence_number),
            ResponseEventRole::CoreStringDelta => self.apply_core_delta(event),
            ResponseEventRole::CoreStringDone => self.complete_channel(event),
            ResponseEventRole::ContentPartAdded => self.add_content_part(event),
            ResponseEventRole::ContentPartDone => self.complete_content_part(event),
            ResponseEventRole::ReasoningSummaryPartAdded => self.add_reasoning_summary_part(event),
            ResponseEventRole::ReasoningSummaryPartDone => {
                self.complete_reasoning_summary_part(event)
            }
            ResponseEventRole::AnnotationAdded => self.add_annotation(event),
            ResponseEventRole::ImagePartial => self.add_image_partial(event),
            ResponseEventRole::ItemStringDelta(kind) => self.append_item_string(event, kind),
            ResponseEventRole::ItemStringDone(kind) => self.complete_item_string(event, kind),
            ResponseEventRole::HostedLifecycle(family, phase) => {
                self.observe_hosted_lifecycle(event, family, phase)
            }
            ResponseEventRole::UnsupportedMedia => {
                Err(ResponseReconciliationError::UnsupportedResponseMedia)
            }
        }
    }

    fn apply_core_delta(
        &mut self,
        event: &SseEvent,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        match event.event_type.as_str() {
            "response.output_text.delta" => {
                self.append_indexed_delta(event, ResponseDeltaChannel::OutputText)
            }
            "response.refusal.delta" => {
                self.append_indexed_delta(event, ResponseDeltaChannel::Refusal)
            }
            "response.reasoning_text.delta" => {
                self.append_indexed_delta(event, ResponseDeltaChannel::ReasoningText)
            }
            "response.reasoning_summary_text.delta" => self.append_summary_delta(event),
            "response.function_call_arguments.delta" => {
                self.append_call_delta(event, ResponseDeltaChannel::FunctionCallArguments)
            }
            "response.custom_tool_call_input.delta" => {
                self.append_call_delta(event, ResponseDeltaChannel::CustomToolCallInput)
            }
            _ => Err(ResponseReconciliationError::UnclassifiedPublicEvent),
        }
    }
}
