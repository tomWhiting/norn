//! Tool trait definition.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::catalog::ToolCatalogEntry;
use super::context::ToolContext;
use super::envelope::ToolEnvelope;
use super::failure::ToolErrorPayload;
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
///
/// Constructed through [`ToolOutput::success`] / [`ToolOutput::failure`] /
/// [`ToolOutput::failure_with_content`] so that the typed failure payload
/// and the model-facing content can never disagree: a failure always
/// carries a [`ToolErrorPayload`] and always surfaces it in `content`
/// under the `error` key (the codebase-wide error convention), so the
/// structured payload survives into the `ToolResult` event for embedders
/// to dispatch on.
///
/// Deliberately `Serialize`-only: deriving `Deserialize` would let a
/// deserialized value carry a `content` that disagrees with `error`,
/// bypassing the constructor invariant above. To rebuild a `ToolOutput`
/// from persisted model-facing content, use [`ToolOutput::from_content`],
/// which re-types the payload from the `error` key.
///
/// `duration` is stamped by the dispatching registry around the execute
/// phase — tools do not measure themselves.
#[derive(Clone, Debug, Serialize)]
pub struct ToolOutput {
    /// Structured content returned by the tool.
    pub content: Value,
    /// Typed failure payload when this result reports an error to the
    /// model; `None` for successful results.
    error: Option<ToolErrorPayload>,
    /// How long the execution took.
    #[serde(serialize_with = "duration_millis::serialize")]
    pub duration: Duration,
}

impl ToolOutput {
    /// A successful result carrying `content`.
    #[must_use]
    pub fn success(content: Value) -> Self {
        Self {
            content,
            error: None,
            duration: Duration::ZERO,
        }
    }

    /// A failed result whose model-facing content is the payload itself,
    /// rendered as `{"error": {kind, message, ...}}`.
    #[must_use]
    pub fn failure(error: ToolErrorPayload) -> Self {
        let content = serde_json::json!({ "error": error.to_value() });
        Self {
            content,
            error: Some(error),
            duration: Duration::ZERO,
        }
    }

    /// A failed result that keeps tool-specific `content` alongside the
    /// payload.
    ///
    /// The payload is injected into object content under the `error` key
    /// (replacing any pre-existing `error` value — the typed payload is
    /// authoritative). Non-object content is wrapped as
    /// `{"_original": <content>, "error": ...}`, mirroring the registry's
    /// advisory-wrapping convention, so the error always reaches the model.
    #[must_use]
    pub fn failure_with_content(content: Value, error: ToolErrorPayload) -> Self {
        let content = match content {
            Value::Object(mut map) => {
                map.insert("error".to_string(), error.to_value());
                Value::Object(map)
            }
            other => serde_json::json!({ "_original": other, "error": error.to_value() }),
        };
        Self {
            content,
            error: Some(error),
            duration: Duration::ZERO,
        }
    }

    /// Reconstruct a `ToolOutput` from dispatched model-facing content
    /// (e.g. for hook envelopes built after dispatch). Detects the
    /// codebase-wide `error`-key convention and re-types the payload via
    /// [`ToolErrorPayload::from_error_value`].
    #[must_use]
    pub fn from_content(content: Value) -> Self {
        let error = content
            .get("error")
            .and_then(ToolErrorPayload::from_error_value);
        Self {
            content,
            error,
            duration: Duration::ZERO,
        }
    }

    /// Whether this result reports an error to the model.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }

    /// The typed failure payload, when this result is an error.
    #[must_use]
    pub fn error(&self) -> Option<&ToolErrorPayload> {
        self.error.as_ref()
    }
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
    /// command's effect per [`ToolEffect::combine`] — a tool with any
    /// disk-mutating command reports at least
    /// [`ToolEffect::Write`](super::scheduling::ToolEffect::Write), one
    /// with any external-system mutation at least
    /// [`ToolEffect::RemoteMutation`](super::scheduling::ToolEffect::RemoteMutation)
    /// — so a caller that cannot inspect the arguments never mis-schedules
    /// a mutation as concurrent. Per-call precision is available through
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

    /// Catalog entries describing this tool for `tool_search`.
    ///
    /// The default derives a single top-level entry from [`Self::name`],
    /// [`Self::description`], and [`Self::input_schema`] — field hints
    /// (names, type hints, required-ness, descriptions, enum values) are
    /// extracted from the schema, which the `ToolArgs` derive in turn
    /// builds from the args struct. Composite tools return one additional
    /// entry per subcommand (see
    /// [`CompositeTool`](super::composite::CompositeTool), whose blanket
    /// impl does this automatically).
    fn catalog_entries(&self) -> Vec<ToolCatalogEntry> {
        vec![ToolCatalogEntry::from_tool_schema(
            self.name(),
            self.description(),
            &self.input_schema(),
        )]
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

    use serde::Serializer;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
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
    use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};

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
            serde_json::json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "Target path." }
                },
                "additionalProperties": false
            })
        }
        fn effect(&self) -> ToolEffect {
            self.0
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::success(serde_json::Value::Null))
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
    fn default_catalog_entries_derive_fields_from_schema() {
        let tool = EffectTool(ToolEffect::Write);
        let entries = tool.catalog_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "effect_tool");
        assert_eq!(entries[0].description, "fixture");
        assert!(entries[0].parent_tool.is_none());
        assert_eq!(entries[0].fields.len(), 1);
        assert_eq!(entries[0].fields[0].name, "path");
        assert_eq!(entries[0].fields[0].type_hint, "string");
        assert!(entries[0].fields[0].required);
        assert_eq!(entries[0].fields[0].description, "Target path.");
    }

    /// `ToolOutput` is Serialize-only (no `Deserialize` back door that
    /// could desync `content` and `error`); the wire form carries the
    /// content, the error payload, and the duration in milliseconds.
    /// Reconstruction happens through `from_content`, which re-types the
    /// error from the content's `error` key.
    #[test]
    fn tool_output_serializes_and_rebuilds_via_from_content() -> Result<(), serde_json::Error> {
        let mut output = ToolOutput::success(serde_json::json!({"result": "ok"}));
        output.duration = Duration::from_millis(42);
        let json = serde_json::to_value(&output)?;
        assert_eq!(json["content"], serde_json::json!({"result": "ok"}));
        assert_eq!(json["error"], serde_json::Value::Null);
        assert_eq!(json["duration"], 42);

        let rebuilt = ToolOutput::from_content(json["content"].clone());
        assert_eq!(rebuilt.content, output.content);
        assert!(!rebuilt.is_error());
        assert!(rebuilt.error().is_none());
        Ok(())
    }

    #[test]
    fn failure_embeds_payload_under_error_key() {
        let payload = ToolErrorPayload::new(ToolErrorKind::NotFound, "no such task")
            .with_detail(serde_json::json!({ "task_id": "t-1" }));
        let output = ToolOutput::failure(payload.clone());
        assert!(output.is_error());
        assert_eq!(output.error(), Some(&payload));
        assert_eq!(output.content["error"]["kind"], "not_found");
        assert_eq!(output.content["error"]["message"], "no such task");
        assert_eq!(output.content["error"]["detail"]["task_id"], "t-1");
    }

    #[test]
    fn failure_with_content_injects_error_into_object_content() {
        let payload = ToolErrorPayload::new(ToolErrorKind::Conflict, "already claimed");
        let output = ToolOutput::failure_with_content(
            serde_json::json!({ "action": "claim", "task_id": "t-2" }),
            payload,
        );
        assert!(output.is_error());
        assert_eq!(output.content["action"], "claim");
        assert_eq!(output.content["task_id"], "t-2");
        assert_eq!(output.content["error"]["kind"], "conflict");
    }

    #[test]
    fn failure_with_content_wraps_non_object_content() {
        let payload = ToolErrorPayload::new(ToolErrorKind::ExecutionFailed, "boom");
        let output = ToolOutput::failure_with_content(serde_json::json!("raw text"), payload);
        assert_eq!(output.content["_original"], "raw text");
        assert_eq!(output.content["error"]["kind"], "execution_failed");
    }

    #[test]
    fn from_content_retypes_error_payloads() {
        let typed = ToolOutput::from_content(serde_json::json!({
            "error": { "kind": "timeout", "message": "took too long" }
        }));
        assert!(typed.is_error());
        assert_eq!(
            typed.error().map(|e| e.kind.clone()),
            Some(ToolErrorKind::Timeout)
        );

        let legacy = ToolOutput::from_content(serde_json::json!({ "error": "plain string" }));
        assert!(legacy.is_error());
        assert_eq!(
            legacy.error().map(|e| e.kind.clone()),
            Some(ToolErrorKind::ExecutionFailed)
        );

        let success = ToolOutput::from_content(serde_json::json!({ "result": "ok" }));
        assert!(!success.is_error());
    }

    /// A failure's typed payload survives serialization and is fully
    /// recoverable from the model-facing content alone via
    /// `from_content` — the supported reconstruction path now that
    /// `ToolOutput` no longer implements `Deserialize`.
    #[test]
    fn failure_payload_recoverable_from_serialized_content() -> Result<(), serde_json::Error> {
        let payload = ToolErrorPayload::new(ToolErrorKind::Custom("member_suspended".into()), "x");
        let output = ToolOutput::failure(payload.clone());
        let json = serde_json::to_value(&output)?;
        assert_eq!(json["error"]["kind"], "member_suspended");

        let rebuilt = ToolOutput::from_content(json["content"].clone());
        assert_eq!(rebuilt.error(), Some(&payload));
        Ok(())
    }
}
