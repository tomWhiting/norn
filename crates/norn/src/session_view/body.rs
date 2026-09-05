//! Typed display-field capabilities and UTF-8 ranges; never arbitrary paths or JSON pointers.

use std::borrow::Cow;
use std::num::NonZeroUsize;

use crate::provider::agent_event::{
    AGENT_MESSAGE_DELIVERED_EVENT_TYPE, AGENT_MESSAGE_SENT_EVENT_TYPE, AgentCompaction,
    AgentMessageLifecycle, COMPACTION_EVENT_TYPE, SUBAGENT_COMPLETED_EVENT_TYPE,
    SUBAGENT_STARTED_EVENT_TYPE, SubagentLifecycle,
};
use crate::provider::reasoning::ReasoningSummaryPart;
use crate::provider::request::ToolCallKind;
use crate::provider::response_item::{ResponseContentPart, ResponseItem};
use crate::session::events::{EventId, SessionEvent};

use super::contract::{HistoryCursor, HistoryPosition, ProvisionalKey};
use super::error::ViewError;

/// Terminal-safe plain data, without ANSI, OSC, C0/C1 or bidi control effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayText(String);

impl DisplayText {
    /// Preserve hard newlines/tabs and visibly escape other control characters.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let mut safe = String::new();
        for character in text.chars() {
            if (character.is_control() && character != '\n' && character != '\t')
                || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                safe.extend(character.escape_unicode());
            } else {
                safe.push(character);
            }
        }
        Self(safe)
    }

    /// Safe text bytes; original soft wrapping is not present in this value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Representation of original approved body bytes, before terminal escaping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BodyRepresentation {
    /// Plain text or an original tool argument string.
    Text,
    /// Serialized JSON; a range is not a decoded JSON-field range.
    Json,
}

/// Closed allowlist of displayable fields in a committed event.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DisplayField {
    /// Stored input text; no inferred operator authority.
    UserContent,
    /// Legacy flat assistant text, only when `response_items` is empty.
    AssistantContent,
    /// Legacy flat display reasoning, never raw reasoning content.
    AssistantThinking,
    /// Approved legacy reasoning summary, only when flat thinking is absent.
    ReasoningSummary {
        /// Reasoning item index.
        item: usize,
        /// Summary part index.
        part: usize,
    },
    /// Exact output-text part in the authoritative item array.
    ResponseText {
        /// Output item index.
        item: usize,
        /// Content part index.
        part: usize,
    },
    /// Exact refusal part in the authoritative item array.
    ResponseRefusal {
        /// Output item index.
        item: usize,
        /// Content part index.
        part: usize,
    },
    /// Approved `summary_text` in a reasoning item; never its content/raw fields.
    ResponseSummary {
        /// Output item index.
        item: usize,
        /// Summary part index.
        part: usize,
    },
    /// Exact function arguments/custom input, retained as its original string.
    ResponseArguments {
        /// Output item index.
        item: usize,
    },
    /// Legacy arguments; JSON objects were already decoded by persistence.
    LegacyArguments {
        /// Call index inside the assistant event.
        call: usize,
    },
    /// Stored inline tool output, possibly a bounded model-facing projection.
    ToolOutputInline,
    /// Full raw serialized JSON spool, resolved only by its owning store.
    ToolOutputSpool,
    /// Validated spoken-response structured content.
    SpokenContent,
    /// Structured child completion result.
    ForkResult,
    /// Committed compaction summary text.
    CompactionSummary,
    /// An explicitly present label description.
    LabelDescription,
    /// Recorded rule injection text, without UI command authority.
    RuleContent,
    /// Strictly decoded known lifecycle record; unknown custom data is excluded.
    CustomLifecycle,
}

/// Typed body identity and revision. No variant carries a filesystem path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BodyOrigin {
    /// An immutable event plus one allowlisted display field.
    Committed {
        /// Exact event position/source.
        cursor: HistoryCursor,
        /// Approved field.
        field: DisplayField,
        /// Original byte representation.
        representation: BodyRepresentation,
    },
    /// Projection-owned volatile bytes at one exact revision.
    Provisional {
        /// Owning source/attempt/segment.
        key: ProvisionalKey,
        /// Exact body revision.
        revision: u64,
        /// Original byte representation.
        representation: BodyRepresentation,
    },
    /// Projection-owned local content outside a provider response attempt.
    Local {
        /// Actual owning source.
        source: super::contract::ViewSource,
        /// Local owning item ordinal.
        ordinal: u64,
        /// Exact revision.
        revision: u64,
        /// Original byte representation.
        representation: BodyRepresentation,
    },
}

/// Opaque display capability minted only inside Norn's store/projection owners.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BodyRef {
    pub(crate) origin: BodyOrigin,
}

impl BodyRef {
    /// Mint a display capability from a validated owning event.
    pub(crate) fn committed(
        cursor: HistoryCursor,
        event: &SessionEvent,
        field: DisplayField,
    ) -> Result<Self, ViewError> {
        if !matches!(&cursor.position, HistoryPosition::Event { event_id, .. } if event_id == &event.base().id)
        {
            return Err(ViewError::CursorMismatch {
                event_id: event.base().id.clone(),
            });
        }
        let representation = validate_display_field(event, &field)?;
        Ok(Self {
            origin: BodyOrigin::Committed {
                cursor,
                field,
                representation,
            },
        })
    }

    /// Inspect a capability; constructing an inspection value cannot mint one.
    #[must_use]
    pub const fn origin(&self) -> &BodyOrigin {
        &self.origin
    }

    /// Original representation, independent of frontend presentation.
    #[must_use]
    pub const fn representation(&self) -> BodyRepresentation {
        match &self.origin {
            BodyOrigin::Committed { representation, .. }
            | BodyOrigin::Provisional { representation, .. }
            | BodyOrigin::Local { representation, .. } => *representation,
        }
    }
}

/// Explicit original-byte demand; there is no hidden whole-body read policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyRange {
    /// Original-content byte offset.
    pub offset: usize,
    /// Caller-declared maximum byte demand.
    pub max_bytes: NonZeroUsize,
}

impl BodyRange {
    /// Select at most the demanded bytes, preserving complete UTF-8 characters.
    /// An end inside a character moves backwards; a split start is refused.
    pub fn slice(self, text: &str) -> Result<(&str, usize), ViewError> {
        if self.offset > text.len() || !text.is_char_boundary(self.offset) {
            return Err(ViewError::InvalidRange {
                offset: self.offset,
            });
        }
        let mut end = self
            .offset
            .saturating_add(self.max_bytes.get())
            .min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == self.offset && end != text.len() {
            return Err(ViewError::RangeTooSmall {
                offset: self.offset,
                demand: self.max_bytes.get(),
            });
        }
        Ok((&text[self.offset..end], end))
    }
}

/// Validate field authority without reading a spool or copying whole body bytes.
pub fn validate_display_field(
    event: &SessionEvent,
    field: &DisplayField,
) -> Result<BodyRepresentation, ViewError> {
    if let (
        SessionEvent::ToolResult {
            spool_ref: Some(_), ..
        },
        DisplayField::ToolOutputSpool,
    ) = (event, field)
    {
        return Ok(BodyRepresentation::Json);
    }
    let value = inline_field(event, field)?;
    Ok(match value {
        InlineField::Text(_) => BodyRepresentation::Text,
        InlineField::Json(_) | InlineField::Lifecycle(_) => BodyRepresentation::Json,
    })
}

/// Resolve approved original bytes off the render/input path.
/// Spools remain the owning store's responsibility; sanitize ranges with
/// [`DisplayText`] before rendering. This never exposes raw provider state.
pub fn resolve_committed_body<'a>(
    event: &'a SessionEvent,
    field: &DisplayField,
) -> Result<Cow<'a, str>, ViewError> {
    if *field == DisplayField::ToolOutputSpool {
        validate_display_field(event, field)?;
        return Err(ViewError::SpoolRequired {
            event_id: event.base().id.clone(),
        });
    }
    match inline_field(event, field)? {
        InlineField::Text(text) => Ok(Cow::Borrowed(text)),
        InlineField::Json(value) => serde_json::to_string(value)
            .map(Cow::Owned)
            .map_err(|source| malformed(event, &source)),
        InlineField::Lifecycle(data) => lifecycle_body(event, data).map(Cow::Owned),
    }
}

enum InlineField<'a> {
    Text(&'a str),
    Json(&'a serde_json::Value),
    Lifecycle(&'a serde_json::Value),
}

fn inline_field<'a>(
    event: &'a SessionEvent,
    field: &DisplayField,
) -> Result<InlineField<'a>, ViewError> {
    let unavailable = || ViewError::FieldUnavailable {
        event_id: event.base().id.clone(),
    };
    match (event, field) {
        (SessionEvent::UserMessage { content, .. }, DisplayField::UserContent)
        | (
            SessionEvent::Compaction {
                summary: content, ..
            },
            DisplayField::CompactionSummary,
        )
        | (
            SessionEvent::Label {
                description: Some(content),
                ..
            },
            DisplayField::LabelDescription,
        )
        | (SessionEvent::RuleInjection { content, .. }, DisplayField::RuleContent) => {
            Ok(InlineField::Text(content))
        }
        (SessionEvent::SpokenResponse { content, .. }, DisplayField::SpokenContent)
        | (
            SessionEvent::ForkComplete {
                result_summary: content,
                ..
            },
            DisplayField::ForkResult,
        )
        | (
            SessionEvent::ToolResult {
                output: content, ..
            },
            DisplayField::ToolOutputInline,
        ) => Ok(InlineField::Json(content)),
        (
            SessionEvent::Custom {
                event_type, data, ..
            },
            DisplayField::CustomLifecycle,
        ) if known_lifecycle(event_type) => Ok(InlineField::Lifecycle(data)),
        (
            SessionEvent::AssistantMessage {
                response_items,
                content,
                thinking,
                reasoning,
                tool_calls,
                ..
            },
            _,
        ) => {
            if response_items.is_empty() {
                return match field {
                    DisplayField::AssistantContent => Ok(InlineField::Text(content)),
                    DisplayField::AssistantThinking => Ok(InlineField::Text(thinking)),
                    DisplayField::ReasoningSummary { item, part } if thinking.is_empty() => {
                        let ReasoningSummaryPart::SummaryText { text } = reasoning
                            .get(*item)
                            .and_then(|item| item.summary.get(*part))
                            .ok_or_else(unavailable)?;
                        Ok(InlineField::Text(text))
                    }
                    DisplayField::LegacyArguments { call } => {
                        let call = tool_calls.get(*call).ok_or_else(unavailable)?;
                        match (&call.kind, &call.arguments) {
                            (ToolCallKind::Custom, serde_json::Value::String(input)) => {
                                Ok(InlineField::Text(input))
                            }
                            (ToolCallKind::Function, arguments) => Ok(InlineField::Json(arguments)),
                            (ToolCallKind::Custom, _) => Err(unavailable()),
                        }
                    }
                    _ => Err(unavailable()),
                };
            }
            response_field(response_items, field).ok_or_else(unavailable)
        }
        _ => Err(unavailable()),
    }
}

fn response_field<'a>(
    items: &'a [crate::provider::response_item::ResponseTranscriptItem],
    field: &DisplayField,
) -> Option<InlineField<'a>> {
    let (DisplayField::ResponseText { item, part }
    | DisplayField::ResponseRefusal { item, part }
    | DisplayField::ResponseSummary { item, part }) = field
    else {
        if let DisplayField::ResponseArguments { item } = field {
            return match &items.get(*item)?.item {
                ResponseItem::FunctionCall(call) => Some(InlineField::Text(call.arguments())),
                ResponseItem::CustomToolCall(call) => Some(InlineField::Text(call.input())),
                _ => None,
            };
        }
        return None;
    };
    match (&items.get(*item)?.item, field) {
        (ResponseItem::Message(message), DisplayField::ResponseText { .. }) => {
            match message.content().get(*part)? {
                ResponseContentPart::OutputText { text, .. } => Some(InlineField::Text(text)),
                _ => None,
            }
        }
        (ResponseItem::Message(message), DisplayField::ResponseRefusal { .. }) => {
            match message.content().get(*part)? {
                ResponseContentPart::Refusal { refusal, .. } => Some(InlineField::Text(refusal)),
                _ => None,
            }
        }
        (ResponseItem::Reasoning(reasoning), DisplayField::ResponseSummary { .. }) => {
            summary_text(reasoning.summary().get(*part)?).map(InlineField::Text)
        }
        _ => None,
    }
}

pub(super) fn summary_text(value: &serde_json::Value) -> Option<&str> {
    (value.get("type")?.as_str()? == "summary_text")
        .then(|| value.get("text")?.as_str())
        .flatten()
}

/// Whether a custom discriminator has an explicit typed display projection.
#[must_use]
pub fn known_lifecycle(event_type: &str) -> bool {
    matches!(
        event_type,
        SUBAGENT_STARTED_EVENT_TYPE
            | SUBAGENT_COMPLETED_EVENT_TYPE
            | AGENT_MESSAGE_SENT_EVENT_TYPE
            | AGENT_MESSAGE_DELIVERED_EVENT_TYPE
            | COMPACTION_EVENT_TYPE
    )
}

fn lifecycle_body(event: &SessionEvent, value: &serde_json::Value) -> Result<String, ViewError> {
    let SessionEvent::Custom { event_type, .. } = event else {
        return Err(ViewError::FieldUnavailable {
            event_id: event.base().id.clone(),
        });
    };
    let result = match event_type.as_str() {
        SUBAGENT_STARTED_EVENT_TYPE | SUBAGENT_COMPLETED_EVENT_TYPE => {
            let decoded = serde_json::from_value::<SubagentLifecycle>(value.clone())
                .map_err(|source| malformed(event, &source))?;
            if decoded.session_event_type() != event_type {
                return Err(ViewError::LifecycleMismatch {
                    event_id: event.base().id.clone(),
                });
            }
            serde_json::to_string(&decoded)
        }
        AGENT_MESSAGE_SENT_EVENT_TYPE | AGENT_MESSAGE_DELIVERED_EVENT_TYPE => {
            let decoded = serde_json::from_value::<AgentMessageLifecycle>(value.clone())
                .map_err(|source| malformed(event, &source))?;
            if decoded.session_event_type() != event_type {
                return Err(ViewError::LifecycleMismatch {
                    event_id: event.base().id.clone(),
                });
            }
            serde_json::to_string(&decoded)
        }
        COMPACTION_EVENT_TYPE => serde_json::from_value::<AgentCompaction>(value.clone())
            .and_then(|event| serde_json::to_string(&event)),
        _ => {
            return Err(ViewError::FieldUnavailable {
                event_id: event.base().id.clone(),
            });
        }
    };
    result.map_err(|source| malformed(event, &source))
}

fn malformed(event: &SessionEvent, source: &serde_json::Error) -> ViewError {
    ViewError::MalformedBody {
        event_id: EventId::clone(&event.base().id),
        category: match source.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "data",
            serde_json::error::Category::Eof => "eof",
        },
        line: source.line(),
        column: source.column(),
    }
}
