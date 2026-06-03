//! Tool trait definition.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::context::ToolContext;
use super::envelope::ToolEnvelope;
use super::follow_up::FollowUpAction;
use super::lifecycle::{PostValidateMode, PostValidateOutcome, PreValidateOutcome};
use super::scheduling::ToolEffect;
use crate::error::ToolError;

/// Grouping category for system prompt generation.
///
/// Tools declare a category so the system prompt builder can group related
/// tools together and generate conditional guidance sections. The category
/// has no effect on tool execution or scheduling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolCategory {
    /// File read, write, edit, and patch operations.
    FileSystem,
    /// Content search, file matching, and AST queries.
    Search,
    /// Shell command execution.
    Shell,
    /// HTTP fetch and web search.
    Web,
    /// Sub-agent spawning, forking, messaging, and coordination.
    Agent,
    /// Language server protocol operations.
    Development,
    /// Inline script execution (Rhai).
    Scripting,
    /// Task tracking and management.
    TaskManagement,
    /// Tool catalogue search and discovery.
    Discovery,
    /// Skill template loading.
    Skills,
    /// Composite Meridian product tools (messaging, source, branch, …).
    Meridian,
    /// Uncategorised or third-party tools.
    General,
}

/// The result of a tool execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Structured content returned by the tool.
    pub content: serde_json::Value,
    /// Whether this result represents an error reported to the model.
    pub is_error: bool,
    /// How long the execution took.
    #[serde(with = "duration_millis")]
    pub duration: Duration,
}

/// The core abstraction for all Norn tools.
///
/// Tools have five lifecycle phases: pre-validate, execute, post-validate,
/// on-success, register-follow-ups. Each of the first four has compile-time
/// (baked into the impl) and runtime (configured in `ToolContext`)
/// components; register-follow-ups is a compile-time-only phase that lets a
/// tool declare the deferred actions available on its result.
///
/// The trait is object-safe for use as `Box<dyn Tool + Send + Sync>`.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool identifier used in LLM tool definitions.
    fn name(&self) -> &str;

    /// Human-readable description included in the LLM tool list.
    fn description(&self) -> &str;

    /// JSON Schema for model-supplied parameters.
    fn input_schema(&self) -> serde_json::Value;

    /// Declared side effect for scheduling.
    ///
    /// Returns the tool's whole-tool effect. For a composite tool whose
    /// commands differ in effect, this is the conservative union of every
    /// command's effect (a tool with any mutating command reports `Write`),
    /// so a caller that cannot inspect the arguments never mis-schedules a
    /// mutation as concurrent. Per-call precision is available through
    /// [`Self::effect_for_args`].
    fn effect(&self) -> ToolEffect;

    /// Per-call side-effect classification.
    ///
    /// Composite tools dispatch several operations through a single
    /// `command` parameter, and those operations differ in effect — a
    /// read-only `inbox` versus a mutating `send`. The static
    /// [`Self::effect`] returns one value for the whole tool and cannot
    /// express that split, so the scheduler consults this method with the
    /// model-supplied arguments to classify an individual call.
    ///
    /// The default delegates to [`Self::effect`], so single-effect tools
    /// need not override it. Composite tools override it to inspect the
    /// arguments and return the effect for the selected operation. An
    /// override must never report a narrower effect than the call truly
    /// has: when the arguments are missing or unrecognised it must return
    /// the most conservative effect the tool can produce, so a mutation is
    /// never mis-scheduled as concurrent.
    fn effect_for_args(&self, _args: &serde_json::Value) -> ToolEffect {
        self.effect()
    }

    /// Grouping category for system prompt generation.
    ///
    /// The system prompt builder groups tools by category and generates
    /// conditional guidance sections. Defaults to [`ToolCategory::General`].
    fn category(&self) -> ToolCategory {
        ToolCategory::General
    }

    /// Extended usage guidance included in the system prompt.
    ///
    /// When present, the system prompt builder includes this text alongside
    /// the tool's description to help the model decide when and how to use
    /// the tool. Should cover when to use, when *not* to use, important
    /// constraints, and preferences over other tools.
    ///
    /// Defaults to `None` — the tool's [`Self::description`] is the only
    /// text the model sees beyond the API-level schema.
    fn usage_guidance(&self) -> Option<&str> {
        None
    }

    /// Default post-validate mode for this tool.
    ///
    /// `Gate` for tools that modify existing valid files (`Edit`, `ApplyPatch`).
    /// `Report` for tools that create new files (`Write`).
    fn post_validate_mode(&self) -> PostValidateMode {
        PostValidateMode::Report
    }

    /// Compile-time pre-validation.
    ///
    /// Default: proceed (no compile-time check).
    async fn pre_validate(
        &self,
        _envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> PreValidateOutcome {
        PreValidateOutcome::Proceed
    }

    /// Execute the tool action.
    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError>;

    /// Compile-time post-validation.
    ///
    /// Default: pass (no compile-time check).
    async fn post_validate(&self, _output: &ToolOutput, _ctx: &ToolContext) -> PostValidateOutcome {
        PostValidateOutcome::Pass
    }

    /// Compile-time on-success follow-up.
    ///
    /// Default: no-op. Failures here do not change the tool result.
    async fn on_success(&self, _output: &ToolOutput, _ctx: &ToolContext) {}

    /// Register the deferred follow-up actions available on this result.
    ///
    /// Runs after on-success on the success path, and after a gate-mode
    /// post-validation failure on the error path, so the tool can inspect the
    /// committed output and context to decide which follow-ups to offer. The
    /// registry attaches the returned actions to the result under a
    /// `follow_ups` key (model-facing subset only) and retains the full
    /// vector for action-log indexing.
    ///
    /// Default: no follow-ups.
    async fn register_follow_ups(
        &self,
        _output: &ToolOutput,
        _ctx: &ToolContext,
    ) -> Vec<FollowUpAction> {
        Vec::new()
    }
}

mod duration_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let ms = u64::deserialize(d)?;
        Ok(Duration::from_millis(ms))
    }
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
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;

    fn _assert_object_safe(_: Box<dyn Tool + Send + Sync>) {}

    struct EffectTool(ToolEffect);

    #[async_trait]
    impl Tool for EffectTool {
        fn name(&self) -> &str {
            "effect_tool"
        }
        fn description(&self) -> &str {
            "fixture"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn effect(&self) -> ToolEffect {
            self.0
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                content: serde_json::Value::Null,
                is_error: false,
                duration: Duration::default(),
            })
        }
    }

    #[test]
    fn effect_for_args_defaults_to_whole_tool_effect() {
        let tool = EffectTool(ToolEffect::Write);
        // No override: the per-call effect mirrors the static effect
        // regardless of the arguments.
        assert_eq!(
            tool.effect_for_args(&serde_json::json!({})),
            ToolEffect::Write
        );
        assert_eq!(
            tool.effect_for_args(&serde_json::json!({ "command": "anything" })),
            ToolEffect::Write,
        );
    }

    #[test]
    fn tool_output_serde_roundtrip() -> Result<(), serde_json::Error> {
        let output = ToolOutput {
            content: serde_json::json!({"result": "ok"}),
            is_error: false,
            duration: Duration::from_millis(42),
        };
        let json = serde_json::to_string(&output)?;
        let parsed: ToolOutput = serde_json::from_str(&json)?;
        assert_eq!(parsed.content, serde_json::json!({"result": "ok"}));
        assert!(!parsed.is_error);
        assert_eq!(parsed.duration, Duration::from_millis(42));
        Ok(())
    }
}
