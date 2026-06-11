//! Pre-validate, post-validate, on-success phases.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::context::ToolContext;
use super::envelope::ToolEnvelope;
use super::failure::{ToolErrorKind, ToolErrorPayload};
use super::traits::ToolOutput;

/// Structured description of why a pre-validate check blocked execution.
///
/// A block carries a machine-readable [`ToolErrorKind`], the human/model
/// facing `message`, optional model-visible `guidance` (what the model
/// should do instead), and free-form machine-readable `detail`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockDecision {
    /// Machine-readable classification of the block.
    pub kind: ToolErrorKind,
    /// Why execution was blocked.
    pub message: String,
    /// Model-visible guidance on how to proceed instead, when the check
    /// has something actionable to say.
    pub guidance: Option<String>,
    /// Free-form machine-readable detail (`Value::Null` when none).
    #[serde(default)]
    pub detail: Value,
}

impl BlockDecision {
    /// Construct a block with [`ToolErrorKind::Blocked`], no guidance, and
    /// no detail.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: ToolErrorKind::Blocked,
            message: message.into(),
            guidance: None,
            detail: Value::Null,
        }
    }

    /// Override the machine-readable kind.
    #[must_use]
    pub fn with_kind(mut self, kind: ToolErrorKind) -> Self {
        self.kind = kind;
        self
    }

    /// Attach model-visible guidance on how to proceed.
    #[must_use]
    pub fn with_guidance(mut self, guidance: impl Into<String>) -> Self {
        self.guidance = Some(guidance.into());
        self
    }

    /// Attach machine-readable detail.
    #[must_use]
    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = detail;
        self
    }

    /// The model-facing rendering: the message, followed by the guidance
    /// when present.
    #[must_use]
    pub fn model_message(&self) -> String {
        match &self.guidance {
            Some(guidance) => format!("{} Guidance: {guidance}", self.message),
            None => self.message.clone(),
        }
    }

    /// Convert into the typed failure payload, folding the guidance into
    /// the payload detail under a `guidance` key so it stays machine
    /// readable.
    #[must_use]
    pub fn into_payload(self) -> ToolErrorPayload {
        let Self {
            kind,
            message,
            guidance,
            detail,
        } = self;
        let detail = match guidance {
            None => detail,
            Some(guidance) => match detail {
                Value::Null => serde_json::json!({ "guidance": guidance }),
                Value::Object(mut map) => {
                    map.insert("guidance".to_string(), Value::String(guidance));
                    Value::Object(map)
                }
                other => serde_json::json!({ "guidance": guidance, "detail": other }),
            },
        };
        ToolErrorPayload {
            kind,
            message,
            detail,
        }
    }
}

impl From<BlockDecision> for crate::error::ToolError {
    /// Convert a block decision into the hard error the dispatch path
    /// returns, preserving the full structure: the payload (kind, message,
    /// detail with guidance folded in) rides on
    /// [`ToolError::PreValidationFailed`](crate::error::ToolError::PreValidationFailed)
    /// and survives verbatim into the persisted `ToolResult` event, while
    /// `Display` still renders the model-facing message-plus-guidance.
    fn from(decision: BlockDecision) -> Self {
        Self::PreValidationFailed {
            payload: decision.into_payload(),
        }
    }
}

/// Result of a pre-validate check.
#[derive(Clone, Debug)]
pub enum PreValidateOutcome {
    /// The tool may proceed with execution.
    Proceed,
    /// Execution is blocked, with a structured reason.
    Block(BlockDecision),
}

impl PreValidateOutcome {
    /// Convenience constructor for a [`ToolErrorKind::Blocked`] block with
    /// no guidance.
    #[must_use]
    pub fn block(message: impl Into<String>) -> Self {
        Self::Block(BlockDecision::new(message))
    }
}

/// Result of a post-validate check.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PostValidateOutcome {
    /// Validation passed.
    Pass,
    /// Validation failed.
    Fail {
        /// Descriptions of what failed.
        errors: Vec<String>,
    },
}

/// Severity for a non-blocking advisory emitted by a post-check.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AdvisorySeverity {
    /// Informational note.
    Info,
    /// Warning that should be surfaced but does not block execution.
    Warning,
}

/// Informational finding emitted by a runtime post-validation check.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Advisory {
    /// Severity of the advisory.
    pub severity: AdvisorySeverity,
    /// Human-readable message to surface to the caller.
    pub message: String,
    /// Check or subsystem that produced the advisory.
    pub source: String,
}

/// Combined result of a runtime post-validation check.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PostCheckResult {
    /// Blocking pass/fail outcome.
    pub outcome: PostValidateOutcome,
    /// Non-blocking advisories to inject into output.
    pub advisories: Vec<Advisory>,
}

impl PostCheckResult {
    /// Convenience constructor for a successful result with no advisories.
    #[must_use]
    pub fn pass() -> Self {
        Self {
            outcome: PostValidateOutcome::Pass,
            advisories: Vec::new(),
        }
    }

    /// Convenience constructor for a failing result with no advisories.
    #[must_use]
    pub fn fail(errors: Vec<String>) -> Self {
        Self {
            outcome: PostValidateOutcome::Fail { errors },
            advisories: Vec::new(),
        }
    }
}

/// Determines how post-validate failures are handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostValidateMode {
    /// Execution staged in memory; rollback on failure.
    /// Used when a valid prior state exists to protect (`Edit`, `ApplyPatch`).
    Gate,
    /// Execution committed; errors reported in tool result.
    /// Used when no valid prior state exists (Write creating new file).
    Report,
}

/// Records when an orchestrator context flag overrides a compile-time check.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckOverride {
    /// Name of the check that was overridden.
    pub check_name: String,
    /// The flag that triggered the override.
    pub flag: super::context::ToolFlag,
    /// Source attribution — read from the `FlagEntry` that triggered the override.
    pub source: String,
}

/// A runtime pre-validate check configured by profile or policy.
#[async_trait]
pub trait RuntimePreValidateCheck: Send + Sync {
    /// Runs the check against the tool envelope and context.
    async fn check(&self, envelope: &ToolEnvelope, ctx: &ToolContext) -> PreValidateOutcome;
}

/// A runtime post-validate check configured by profile or policy.
#[async_trait]
pub trait RuntimePostValidateCheck: Send + Sync {
    /// Runs the check against the tool output and context.
    async fn check(&self, output: &ToolOutput, ctx: &ToolContext) -> PostCheckResult;
}

/// A runtime on-success action configured by profile or policy.
///
/// Failures in on-success actions do not modify the tool result.
#[async_trait]
pub trait RuntimeOnSuccessAction: Send + Sync {
    /// Runs the follow-up action after a successful tool execution.
    async fn run(&self, output: &ToolOutput, ctx: &ToolContext);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn block_decision_model_message_includes_guidance() {
        let plain = BlockDecision::new("file has not been read");
        assert_eq!(plain.model_message(), "file has not been read");

        let guided = BlockDecision::new("file has not been read")
            .with_guidance("read the file with the read tool first");
        assert_eq!(
            guided.model_message(),
            "file has not been read Guidance: read the file with the read tool first",
        );
    }

    #[test]
    fn block_decision_into_payload_folds_guidance_into_detail() {
        let payload = BlockDecision::new("path escapes workspace")
            .with_kind(ToolErrorKind::PermissionDenied)
            .with_detail(serde_json::json!({ "path": "/etc/passwd" }))
            .with_guidance("use a path inside the workspace root")
            .into_payload();
        assert_eq!(payload.kind, ToolErrorKind::PermissionDenied);
        assert_eq!(payload.message, "path escapes workspace");
        assert_eq!(payload.detail["path"], "/etc/passwd");
        assert_eq!(
            payload.detail["guidance"],
            "use a path inside the workspace root"
        );
    }

    #[test]
    fn block_decision_into_tool_error_preserves_structure_and_display() {
        let err: crate::error::ToolError = BlockDecision::new("file has not been read")
            .with_guidance("read the file first")
            .with_detail(serde_json::json!({ "path": "a.rs" }))
            .into();
        let crate::error::ToolError::PreValidationFailed { payload } = &err else {
            panic!("expected PreValidationFailed, got {err:?}");
        };
        assert_eq!(payload.kind, ToolErrorKind::Blocked);
        assert_eq!(payload.message, "file has not been read");
        assert_eq!(payload.guidance(), Some("read the file first"));
        assert_eq!(payload.detail["path"], "a.rs");
        assert_eq!(
            err.to_string(),
            "pre-validation failed: file has not been read Guidance: read the file first",
        );
    }

    #[test]
    fn block_decision_into_payload_without_guidance_keeps_detail_untouched() {
        let payload = BlockDecision::new("blocked").into_payload();
        assert_eq!(payload.kind, ToolErrorKind::Blocked);
        assert!(payload.detail.is_null());

        let scalar_detail = BlockDecision::new("blocked")
            .with_detail(serde_json::json!(["a", "b"]))
            .with_guidance("retry")
            .into_payload();
        assert_eq!(scalar_detail.detail["guidance"], "retry");
        assert_eq!(
            scalar_detail.detail["detail"],
            serde_json::json!(["a", "b"])
        );
    }
}
