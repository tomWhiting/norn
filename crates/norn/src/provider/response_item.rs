//! Lossless completed-item model for the `OpenAI` Responses API.
//!
//! The provider item JSON is authoritative. Typed variants expose the core
//! fields Norn needs for projections and execution while retaining the exact
//! object, including fields added by a newer backend. Stream coordinates live
//! in [`ResponseStreamProvenance`], never inside replayable item JSON.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

mod known;
mod parse;

pub use known::{KnownResponseItem, KnownResponseItemKind};

/// A completed Responses output item.
#[derive(Clone, Debug, PartialEq)]
pub enum ResponseItem {
    /// An assistant message with ordered, typed content parts.
    Message(ResponseMessageItem),
    /// A reasoning item, retaining unknown summary/content parts.
    Reasoning(ResponseReasoningItem),
    /// A structured function call.
    FunctionCall(ResponseFunctionCallItem),
    /// A freeform custom-tool call.
    CustomToolCall(ResponseCustomToolCallItem),
    /// A provider-hosted web-search call.
    WebSearchCall(ResponseWebSearchCallItem),
    /// A provider compaction record.
    Compaction(ResponseCompactionItem),
    /// A pinned public item retained losslessly without a core projection.
    Known(KnownResponseItem),
    /// A valid item whose discriminator is not implemented by this client.
    Opaque(OpaqueResponseItem),
}

/// A message output item.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseMessageItem {
    raw: Value,
    id: String,
    role: String,
    status: String,
    phase: ResponseNullable<String>,
    content: Vec<ResponseContentPart>,
}

/// A provider field for which absence and explicit JSON `null` differ.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ResponseNullable<T> {
    /// The object did not contain the field.
    #[default]
    Absent,
    /// The object contained the field with JSON `null`.
    Null,
    /// The object contained a typed value.
    Value(T),
}

/// One ordered content part inside a response message.
#[derive(Clone, Debug, PartialEq)]
pub enum ResponseContentPart {
    /// Visible model text and its provider annotations.
    OutputText {
        /// Complete text for this part.
        text: String,
        /// Annotation objects in provider order.
        annotations: Vec<Value>,
        /// Token log-probability objects in provider order.
        logprobs: Vec<Value>,
        /// Exact provider object.
        raw: Value,
    },
    /// A model refusal, distinct from ordinary text and transport failure.
    Refusal {
        /// Complete refusal content.
        refusal: String,
        /// Exact provider object.
        raw: Value,
    },
    /// A valid but unsupported content part retained without interpretation.
    Opaque {
        /// Provider discriminator.
        part_type: String,
        /// Exact provider object.
        raw: Value,
    },
}

/// A reasoning output item.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseReasoningItem {
    raw: Value,
    id: String,
    summary: Vec<Value>,
    content: Option<Vec<Value>>,
    encrypted_content: ResponseNullable<String>,
    status: Option<String>,
}

/// A structured function-call output item.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseFunctionCallItem {
    raw: Value,
    id: Option<String>,
    call_id: String,
    name: String,
    arguments: String,
}

/// A freeform custom-tool-call output item.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseCustomToolCallItem {
    raw: Value,
    id: Option<String>,
    call_id: String,
    name: String,
    input: String,
}

/// A provider-hosted web-search output item.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseWebSearchCallItem {
    raw: Value,
    id: String,
    status: String,
    action: Value,
}

/// A provider compaction output item.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseCompactionItem {
    raw: Value,
    item_type: String,
    id: String,
    encrypted_content: String,
}

/// An unimplemented output item preserved as exact JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct OpaqueResponseItem {
    raw: Value,
    item_type: String,
    id: Option<String>,
}

/// Stream-only coordinates for one completed item.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseStreamProvenance {
    /// Item identifier carried by the stream envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// Position in `response.output`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_index: Option<u64>,
    /// Position within a message's content vector, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_index: Option<u64>,
    /// Provider stream sequence number, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_number: Option<u64>,
}

/// A replayable item paired with non-replayable stream provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResponseTranscriptItem {
    /// Provider item JSON, classified without losing unknown fields.
    pub item: ResponseItem,
    /// Coordinates used for reconciliation and diagnostics only.
    #[serde(default, skip_serializing_if = "is_default_provenance")]
    pub provenance: ResponseStreamProvenance,
}

/// Structural error in a known Responses item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResponseItemError {
    reason: &'static str,
}

impl ResponseItemError {
    const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

impl fmt::Display for ResponseItemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for ResponseItemError {}

impl ResponseItem {
    /// Classify a raw provider item while retaining its complete JSON object.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is not an object, has no discriminator,
    /// or a known variant omits a required field.
    pub fn from_value(raw: Value) -> Result<Self, ResponseItemError> {
        parse::response_item(raw)
    }

    /// Return the provider discriminator.
    #[must_use]
    pub fn item_type(&self) -> &str {
        match self {
            Self::Message(_) => "message",
            Self::Reasoning(_) => "reasoning",
            Self::FunctionCall(_) => "function_call",
            Self::CustomToolCall(_) => "custom_tool_call",
            Self::WebSearchCall(_) => "web_search_call",
            Self::Compaction(item) => &item.item_type,
            Self::Known(item) => item.kind.as_str(),
            Self::Opaque(item) => &item.item_type,
        }
    }

    /// Return the provider item identifier, when present.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Message(item) => Some(item.id.as_str()),
            Self::Reasoning(item) => Some(item.id.as_str()),
            Self::FunctionCall(item) => item.id.as_deref(),
            Self::CustomToolCall(item) => item.id.as_deref(),
            Self::WebSearchCall(item) => Some(item.id.as_str()),
            Self::Compaction(item) => Some(item.id.as_str()),
            Self::Known(item) => item.id.as_deref(),
            Self::Opaque(item) => item.id.as_deref(),
        }
    }

    /// Return the exact replayable provider item JSON.
    #[must_use]
    pub fn raw(&self) -> &Value {
        match self {
            Self::Message(item) => &item.raw,
            Self::Reasoning(item) => &item.raw,
            Self::FunctionCall(item) => &item.raw,
            Self::CustomToolCall(item) => &item.raw,
            Self::WebSearchCall(item) => &item.raw,
            Self::Compaction(item) => &item.raw,
            Self::Known(item) => &item.raw,
            Self::Opaque(item) => &item.raw,
        }
    }

    /// Return a typed message view when this is a message item.
    #[must_use]
    pub const fn as_message(&self) -> Option<&ResponseMessageItem> {
        match self {
            Self::Message(item) => Some(item),
            _ => None,
        }
    }

    /// Return a typed reasoning view when this is a reasoning item.
    #[must_use]
    pub const fn as_reasoning(&self) -> Option<&ResponseReasoningItem> {
        match self {
            Self::Reasoning(item) => Some(item),
            _ => None,
        }
    }

    /// Return a typed function-call view when present.
    #[must_use]
    pub const fn as_function_call(&self) -> Option<&ResponseFunctionCallItem> {
        match self {
            Self::FunctionCall(item) => Some(item),
            _ => None,
        }
    }

    /// Return a typed custom-tool-call view when present.
    #[must_use]
    pub const fn as_custom_tool_call(&self) -> Option<&ResponseCustomToolCallItem> {
        match self {
            Self::CustomToolCall(item) => Some(item),
            _ => None,
        }
    }
}

impl ResponseMessageItem {
    /// Assistant role carried by the provider output item.
    #[must_use]
    pub fn role(&self) -> &str {
        &self.role
    }

    /// Provider lifecycle status carried by the completed message item.
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Optional provider message phase, preserving absence and explicit null.
    #[must_use]
    pub fn phase(&self) -> ResponseNullable<&str> {
        match &self.phase {
            ResponseNullable::Absent => ResponseNullable::Absent,
            ResponseNullable::Null => ResponseNullable::Null,
            ResponseNullable::Value(value) => ResponseNullable::Value(value),
        }
    }

    /// Ordered message content parts.
    #[must_use]
    pub fn content(&self) -> &[ResponseContentPart] {
        &self.content
    }
}

impl ResponseReasoningItem {
    /// Ordered reasoning summary parts as exact JSON objects.
    #[must_use]
    pub fn summary(&self) -> &[Value] {
        &self.summary
    }

    /// Ordered reasoning content parts as exact JSON objects.
    #[must_use]
    pub fn content(&self) -> Option<&[Value]> {
        self.content.as_deref()
    }

    /// Encrypted reasoning state used by stateless replay.
    #[must_use]
    pub fn encrypted_content(&self) -> Option<&str> {
        match &self.encrypted_content {
            ResponseNullable::Value(value) => Some(value),
            ResponseNullable::Absent | ResponseNullable::Null => None,
        }
    }

    /// Encrypted reasoning state with absence and explicit `null` kept distinct.
    #[must_use]
    pub fn encrypted_content_field(&self) -> ResponseNullable<&str> {
        match &self.encrypted_content {
            ResponseNullable::Absent => ResponseNullable::Absent,
            ResponseNullable::Null => ResponseNullable::Null,
            ResponseNullable::Value(value) => ResponseNullable::Value(value),
        }
    }

    /// Provider lifecycle status, when present.
    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }
}

impl ResponseFunctionCallItem {
    /// Provider correlation identifier for the tool result.
    #[must_use]
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Function name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Raw JSON argument string.
    #[must_use]
    pub fn arguments(&self) -> &str {
        &self.arguments
    }
}

impl ResponseCustomToolCallItem {
    /// Provider correlation identifier for the tool result.
    #[must_use]
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Custom tool name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Freeform custom-tool input.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl ResponseWebSearchCallItem {
    /// Provider lifecycle status.
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Exact hosted-search action object.
    #[must_use]
    pub const fn action(&self) -> &Value {
        &self.action
    }
}

impl ResponseCompactionItem {
    /// Required encrypted provider compaction state.
    #[must_use]
    pub fn encrypted_content(&self) -> &str {
        &self.encrypted_content
    }
}

impl OpaqueResponseItem {
    /// Provider discriminator retained for capability checks.
    #[must_use]
    pub fn item_type(&self) -> &str {
        &self.item_type
    }
}

impl Serialize for ResponseItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.raw().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ResponseItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        Self::from_value(raw).map_err(serde::de::Error::custom)
    }
}

fn is_default_provenance(value: &ResponseStreamProvenance) -> bool {
    *value == ResponseStreamProvenance::default()
}

#[cfg(test)]
mod response_item_tests;
