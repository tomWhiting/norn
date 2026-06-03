//! Pre-validate, post-validate, on-success phases.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::context::ToolContext;
use super::envelope::ToolEnvelope;
use super::traits::ToolOutput;

/// Result of a pre-validate check.
#[derive(Clone, Debug)]
pub enum PreValidateOutcome {
    /// The tool may proceed with execution.
    Proceed,
    /// Execution is blocked.
    Block {
        /// Why execution was blocked.
        reason: String,
    },
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
