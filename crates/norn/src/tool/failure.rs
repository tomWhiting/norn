//! Typed tool-failure payloads.
//!
//! [`ToolErrorPayload`] is the structured failure envelope a tool attaches
//! to a failed [`ToolOutput`](super::traits::ToolOutput): a machine-readable
//! [`ToolErrorKind`], a human/model-facing message, and an optional
//! free-form `detail` value. The payload is embedded into the tool's
//! model-facing content under the `error` key (the codebase-wide error
//! convention) so it survives verbatim into the `ToolResult` event, letting
//! embedders dispatch on `error.kind` instead of parsing prose.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::error::ToolError;

/// Machine-readable classification of a tool failure.
///
/// The named variants cover the failure classes norn's own tools produce
/// and the common classes embedder tools (databases, brokers, remote
/// services) need; [`ToolErrorKind::Custom`] lets an embedder introduce a
/// domain-specific kind without forking the enum. Kinds serialize as plain
/// `snake_case` strings (`"invalid_arguments"`, `"not_found"`, …); a
/// `Custom` kind serializes as its own string. A custom string equal to a
/// named kind's wire form deserializes back to the named kind, so custom
/// kinds must not reuse the named spellings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolErrorKind {
    /// The model-supplied arguments were malformed, missing, or named an
    /// unknown command.
    InvalidArguments,
    /// A required [`ToolContext`](super::context::ToolContext) extension
    /// was not published by the embedder.
    MissingExtension,
    /// A referenced entity (file, task, agent, record) does not exist.
    NotFound,
    /// A pre-validation check or policy blocked the call before execution.
    Blocked,
    /// A post-execution validation check failed.
    ValidationFailed,
    /// The caller lacks permission for the requested operation.
    PermissionDenied,
    /// The operation conflicts with current state (duplicate, already
    /// claimed, concurrent modification).
    Conflict,
    /// The operation exceeded its time budget.
    Timeout,
    /// A local I/O operation failed.
    Io,
    /// A network operation failed.
    Network,
    /// A remote service reported an error.
    ExternalService,
    /// The tool's execution phase failed for a reason not covered by a
    /// more specific kind.
    ExecutionFailed,
    /// An embedder-defined kind, carried as its own wire string.
    Custom(String),
}

impl ToolErrorKind {
    /// The wire string this kind serializes as.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::InvalidArguments => "invalid_arguments",
            Self::MissingExtension => "missing_extension",
            Self::NotFound => "not_found",
            Self::Blocked => "blocked",
            Self::ValidationFailed => "validation_failed",
            Self::PermissionDenied => "permission_denied",
            Self::Conflict => "conflict",
            Self::Timeout => "timeout",
            Self::Io => "io",
            Self::Network => "network",
            Self::ExternalService => "external_service",
            Self::ExecutionFailed => "execution_failed",
            Self::Custom(name) => name,
        }
    }

    /// Parse a wire string back into a kind. Named spellings resolve to
    /// their named variant; anything else becomes [`Self::Custom`].
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            "invalid_arguments" => Self::InvalidArguments,
            "missing_extension" => Self::MissingExtension,
            "not_found" => Self::NotFound,
            "blocked" => Self::Blocked,
            "validation_failed" => Self::ValidationFailed,
            "permission_denied" => Self::PermissionDenied,
            "conflict" => Self::Conflict,
            "timeout" => Self::Timeout,
            "io" => Self::Io,
            "network" => Self::Network,
            "external_service" => Self::ExternalService,
            "execution_failed" => Self::ExecutionFailed,
            other => Self::Custom(other.to_string()),
        }
    }
}

impl std::fmt::Display for ToolErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ToolErrorKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ToolErrorKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw.is_empty() {
            return Err(D::Error::custom("tool error kind must not be empty"));
        }
        Ok(Self::from_wire(&raw))
    }
}

/// Structured failure payload attached to a failed tool result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolErrorPayload {
    /// Machine-readable failure classification.
    pub kind: ToolErrorKind,
    /// Human/model-facing description of the failure.
    pub message: String,
    /// Free-form machine-readable detail (`Value::Null` when none).
    #[serde(default)]
    pub detail: Value,
}

impl ToolErrorPayload {
    /// Construct a payload with no detail.
    #[must_use]
    pub fn new(kind: ToolErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: Value::Null,
        }
    }

    /// Attach machine-readable detail to the payload.
    #[must_use]
    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = detail;
        self
    }

    /// Model-visible guidance carried under `detail.guidance`, when the
    /// originating block attached any (see
    /// [`BlockDecision::into_payload`](super::lifecycle::BlockDecision::into_payload)).
    #[must_use]
    pub fn guidance(&self) -> Option<&str> {
        self.detail.get("guidance").and_then(Value::as_str)
    }

    /// The model-facing rendering: the message, followed by the guidance
    /// when present. Matches
    /// [`BlockDecision::model_message`](super::lifecycle::BlockDecision::model_message)
    /// for payloads produced from a block decision.
    #[must_use]
    pub fn model_message(&self) -> String {
        match self.guidance() {
            Some(guidance) => format!("{} Guidance: {guidance}", self.message),
            None => self.message.clone(),
        }
    }

    /// The model-facing JSON form embedded under a tool result's `error`
    /// key: `{"kind": ..., "message": ...}` plus `detail` when present.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "kind".to_string(),
            Value::String(self.kind.as_str().to_string()),
        );
        map.insert("message".to_string(), Value::String(self.message.clone()));
        if !self.detail.is_null() {
            map.insert("detail".to_string(), self.detail.clone());
        }
        Value::Object(map)
    }

    /// Reconstruct a payload from the model-facing `error` value of a tool
    /// result.
    ///
    /// An object with a string `kind` and `message` round-trips exactly. A
    /// bare string (the legacy collapsed rendering — norn's own dispatch
    /// paths no longer produce it, but embedder tools that hand-build an
    /// `error` value still can) becomes an
    /// [`ToolErrorKind::ExecutionFailed`] payload carrying that string as
    /// its message. Anything else is not a recognisable error value and
    /// yields `None`.
    #[must_use]
    pub fn from_error_value(value: &Value) -> Option<Self> {
        match value {
            Value::String(message) => Some(Self::new(ToolErrorKind::ExecutionFailed, message)),
            Value::Object(map) => {
                let kind = map.get("kind").and_then(Value::as_str)?;
                let message = map.get("message").and_then(Value::as_str)?;
                Some(Self {
                    kind: ToolErrorKind::from_wire(kind),
                    message: message.to_string(),
                    detail: map.get("detail").cloned().unwrap_or(Value::Null),
                })
            }
            _ => None,
        }
    }
}

impl From<&ToolError> for ToolErrorPayload {
    /// Classify a hard [`ToolError`] into the typed payload vocabulary so
    /// the dispatch layer can emit structured `error` values instead of
    /// collapsed strings.
    fn from(error: &ToolError) -> Self {
        match error {
            ToolError::PreValidationFailed { payload } => payload.clone(),
            ToolError::ExecutionFailed { reason } => {
                Self::new(ToolErrorKind::ExecutionFailed, reason.clone())
            }
            ToolError::PostValidationFailed {
                reason,
                committed_output,
            } => {
                let payload = Self::new(ToolErrorKind::ValidationFailed, reason.clone());
                match committed_output {
                    Some(output) => payload.with_detail(serde_json::json!({
                        "committed_output": output.clone(),
                    })),
                    None => payload,
                }
            }
            ToolError::ToolNotFound { name } => {
                Self::new(ToolErrorKind::NotFound, format!("tool not found: {name}"))
                    .with_detail(serde_json::json!({ "tool": name }))
            }
            ToolError::MissingExtension { extension } => Self::new(
                ToolErrorKind::MissingExtension,
                format!("required tool-context extension not configured: {extension}"),
            )
            .with_detail(serde_json::json!({ "extension": extension })),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn kind_wire_round_trip_named_and_custom() {
        let named = [
            ToolErrorKind::InvalidArguments,
            ToolErrorKind::MissingExtension,
            ToolErrorKind::NotFound,
            ToolErrorKind::Blocked,
            ToolErrorKind::ValidationFailed,
            ToolErrorKind::PermissionDenied,
            ToolErrorKind::Conflict,
            ToolErrorKind::Timeout,
            ToolErrorKind::Io,
            ToolErrorKind::Network,
            ToolErrorKind::ExternalService,
            ToolErrorKind::ExecutionFailed,
        ];
        for kind in named {
            assert_eq!(ToolErrorKind::from_wire(kind.as_str()), kind);
            let json = serde_json::to_value(&kind).unwrap();
            let parsed: ToolErrorKind = serde_json::from_value(json).unwrap();
            assert_eq!(parsed, kind);
        }

        let custom = ToolErrorKind::Custom("member_suspended".to_string());
        assert_eq!(custom.as_str(), "member_suspended");
        let json = serde_json::to_value(&custom).unwrap();
        assert_eq!(json, serde_json::json!("member_suspended"));
        assert_eq!(
            serde_json::from_value::<ToolErrorKind>(json).unwrap(),
            custom
        );
    }

    #[test]
    fn empty_kind_string_rejected_on_deserialize() {
        let err = serde_json::from_value::<ToolErrorKind>(serde_json::json!(""))
            .expect_err("empty kind must be rejected");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn payload_model_message_appends_guidance_from_detail() {
        let plain = ToolErrorPayload::new(ToolErrorKind::Blocked, "file has not been read");
        assert_eq!(plain.guidance(), None);
        assert_eq!(plain.model_message(), "file has not been read");

        let guided = plain.with_detail(serde_json::json!({ "guidance": "read it first" }));
        assert_eq!(guided.guidance(), Some("read it first"));
        assert_eq!(
            guided.model_message(),
            "file has not been read Guidance: read it first",
        );
    }

    #[test]
    fn pre_validation_error_display_renders_message_and_guidance() {
        let err = ToolError::PreValidationFailed {
            payload: ToolErrorPayload::new(ToolErrorKind::Blocked, "file has not been read")
                .with_detail(serde_json::json!({ "guidance": "read it first" })),
        };
        assert_eq!(
            err.to_string(),
            "pre-validation failed: file has not been read Guidance: read it first",
        );
    }

    #[test]
    fn payload_to_value_omits_null_detail() {
        let payload = ToolErrorPayload::new(ToolErrorKind::NotFound, "task not found");
        assert_eq!(
            payload.to_value(),
            serde_json::json!({ "kind": "not_found", "message": "task not found" })
        );

        let detailed = payload.with_detail(serde_json::json!({ "task_id": "t-1" }));
        assert_eq!(
            detailed.to_value(),
            serde_json::json!({
                "kind": "not_found",
                "message": "task not found",
                "detail": { "task_id": "t-1" }
            })
        );
    }

    #[test]
    fn payload_from_error_value_round_trips_object_form() {
        let payload = ToolErrorPayload::new(ToolErrorKind::Conflict, "already claimed")
            .with_detail(serde_json::json!({ "task_id": "t-9" }));
        let reparsed = ToolErrorPayload::from_error_value(&payload.to_value())
            .expect("object error value parses");
        assert_eq!(reparsed, payload);
    }

    #[test]
    fn payload_from_error_value_handles_legacy_string_form() {
        let value = serde_json::json!("execution failed: boom");
        let payload = ToolErrorPayload::from_error_value(&value).expect("string form parses");
        assert_eq!(payload.kind, ToolErrorKind::ExecutionFailed);
        assert_eq!(payload.message, "execution failed: boom");
        assert!(payload.detail.is_null());
    }

    #[test]
    fn payload_from_error_value_rejects_non_error_shapes() {
        assert!(ToolErrorPayload::from_error_value(&serde_json::json!(42)).is_none());
        assert!(ToolErrorPayload::from_error_value(&serde_json::json!(null)).is_none());
        assert!(
            ToolErrorPayload::from_error_value(&serde_json::json!({ "kind": "x" })).is_none(),
            "object without message is not an error payload",
        );
    }

    #[test]
    fn payload_from_tool_error_classifies_every_variant() {
        let pre = ToolError::PreValidationFailed {
            payload: ToolErrorPayload::new(ToolErrorKind::PermissionDenied, "must read first")
                .with_detail(serde_json::json!({ "guidance": "read the file first" })),
        };
        let pre_payload = ToolErrorPayload::from(&pre);
        assert_eq!(
            pre_payload.kind,
            ToolErrorKind::PermissionDenied,
            "pre-validation carries the originating payload's kind verbatim",
        );
        assert_eq!(pre_payload.message, "must read first");
        assert_eq!(
            pre_payload.guidance(),
            Some("read the file first"),
            "guidance survives the round-trip through ToolError",
        );

        let exec = ToolError::ExecutionFailed {
            reason: "boom".to_string(),
        };
        assert_eq!(
            ToolErrorPayload::from(&exec).kind,
            ToolErrorKind::ExecutionFailed
        );

        let post = ToolError::PostValidationFailed {
            reason: "broken ast".to_string(),
            committed_output: Some(serde_json::json!({ "committed": true })),
        };
        let post_payload = ToolErrorPayload::from(&post);
        assert_eq!(post_payload.kind, ToolErrorKind::ValidationFailed);
        assert_eq!(
            post_payload.detail["committed_output"]["committed"],
            serde_json::json!(true)
        );

        let not_found = ToolError::ToolNotFound {
            name: "ghost".to_string(),
        };
        let nf_payload = ToolErrorPayload::from(&not_found);
        assert_eq!(nf_payload.kind, ToolErrorKind::NotFound);
        assert_eq!(nf_payload.detail["tool"], serde_json::json!("ghost"));

        let missing = ToolError::MissingExtension {
            extension: "norn::tools::task::SharedTaskStore".to_string(),
        };
        let missing_payload = ToolErrorPayload::from(&missing);
        assert_eq!(missing_payload.kind, ToolErrorKind::MissingExtension);
        assert_eq!(
            missing_payload.detail["extension"],
            serde_json::json!("norn::tools::task::SharedTaskStore")
        );
    }
}
