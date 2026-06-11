//! First-class composite tools.
//!
//! A composite tool exposes several operations behind a single tool name,
//! dispatched by a command field (`action`, `command`, …). Implementing
//! [`CompositeTool`] instead of hand-rolling the pattern on [`Tool`]
//! derives everything the pattern otherwise duplicates:
//!
//! * **Schema** — the command enum derives `ToolArgs` (internally tagged
//!   via `#[serde(tag = "...")]`), so the `oneOf` schema, per-command
//!   required fields, and descriptions come from the enum definition.
//! * **Per-call effects** — [`CompositeTool::command_effect`] matches on
//!   the *typed* command enum, so adding a variant without classifying
//!   its effect is a compile error (the drift the hand-rolled pattern
//!   invites). `effect_for_args` deserializes the raw arguments and falls
//!   back to [`CompositeTool::conservative_effect`] when they don't parse.
//! * **Catalog entries** — one subcommand entry per command, with field
//!   hints extracted from the schema
//!   (see [`composite_commands`](super::catalog::composite_commands)).
//! * **Invalid-command handling** — arguments that don't deserialize into
//!   the command enum produce a typed
//!   [`ToolErrorKind::InvalidArguments`] failure naming the valid
//!   commands, returned as a soft (model-correctable) tool result.
//!
//! Every [`CompositeTool`] automatically implements [`Tool`] through the
//! blanket impl; a type implements one or the other, never both.

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::catalog::{ToolCatalogEntry, composite_commands};
use super::context::ToolContext;
use super::envelope::ToolEnvelope;
use super::failure::{ToolErrorKind, ToolErrorPayload};
use super::follow_up::FollowUpAction;
use super::lifecycle::{PostValidateMode, PostValidateOutcome, PreValidateOutcome};
use super::scheduling::ToolEffect;
use super::traits::{Tool, ToolCategory, ToolOutput};
use crate::error::ToolError;

/// A tool built from typed sub-commands.
///
/// `Command` is an internally-tagged enum (`#[serde(tag = "<command
/// field>")]`) deriving `Deserialize` and `ToolArgs`; its tag field name
/// must equal [`Self::command_field`] and its `json_schema()` is returned
/// from [`Self::input_schema`].
#[async_trait]
pub trait CompositeTool: Send + Sync {
    /// The command enum the model's arguments deserialize into.
    type Command: DeserializeOwned + Send;

    /// Tool identifier used in LLM tool definitions.
    fn name(&self) -> &str;

    /// Human-readable description included in the LLM tool list.
    fn description(&self) -> &str;

    /// The JSON field that selects the command (the enum's serde tag).
    fn command_field(&self) -> &str;

    /// JSON Schema for the command enum — `Self::Command::json_schema()`
    /// when the enum derives `ToolArgs`.
    fn input_schema(&self) -> Value;

    /// Side-effect classification for one *typed* command.
    ///
    /// Matching on the enum makes effect classification exhaustive: a new
    /// command variant cannot be added without declaring its effect.
    fn command_effect(&self, command: &Self::Command) -> ToolEffect;

    /// The effect reported when a call's command cannot be determined
    /// (whole-tool scheduling, malformed arguments).
    ///
    /// # Contract (load-bearing — read before implementing)
    ///
    /// The returned effect MUST be at least as conservative (per
    /// [`ToolEffect::combine`]) as **every** value [`Self::command_effect`]
    /// can return: for each command `c`,
    /// `self.conservative_effect().combine(self.command_effect(&c))` must
    /// equal `self.conservative_effect()`. Violating this lets the
    /// scheduler classify a mutating call as concurrent whenever its
    /// arguments fail to parse (or when only the whole-tool effect is
    /// consulted), so two mutations can race — in this codebase that means
    /// corrupted task stores, double-applied remote mutations, and
    /// conflicting file writes.
    ///
    /// Adding a command variant therefore requires re-checking this
    /// method, not just [`Self::command_effect`]. The contract cannot be
    /// enforced at registration time: the registry holds the tool as
    /// `dyn Tool`, where `Command` is erased, and `Command:
    /// DeserializeOwned` provides no way to enumerate the enum's variants
    /// — so there is nothing to fold [`Self::command_effect`] over at
    /// runtime. Instead, every implementation MUST pin the contract in a
    /// unit test that calls
    /// [`assert_conservative_effect_covers_all_commands`] with one
    /// constructed value per command variant (see the `task` tool's
    /// `conservative_effect_covers_every_command` test for the pattern).
    fn conservative_effect(&self) -> ToolEffect;

    /// Grouping category for system prompt generation.
    fn category(&self) -> ToolCategory {
        ToolCategory::General
    }

    /// Extended usage guidance included in the system prompt.
    fn usage_guidance(&self) -> Option<&str> {
        None
    }

    /// Default post-validate mode for this tool.
    fn post_validate_mode(&self) -> PostValidateMode {
        PostValidateMode::Report
    }

    /// Compile-time pre-validation. Default: proceed.
    async fn pre_validate(
        &self,
        _envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> PreValidateOutcome {
        PreValidateOutcome::Proceed
    }

    /// Execute one typed command.
    async fn run(
        &self,
        command: Self::Command,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError>;

    /// Compile-time post-validation. Default: pass.
    async fn post_validate(&self, _output: &ToolOutput, _ctx: &ToolContext) -> PostValidateOutcome {
        PostValidateOutcome::Pass
    }

    /// Compile-time on-success follow-up. Default: no-op.
    async fn on_success(&self, _output: &ToolOutput, _ctx: &ToolContext) {}

    /// Register the deferred follow-up actions available on this result.
    /// Default: none.
    async fn register_follow_ups(
        &self,
        _output: &ToolOutput,
        _ctx: &ToolContext,
    ) -> Vec<FollowUpAction> {
        Vec::new()
    }
}

/// Verify [`CompositeTool::conservative_effect`]'s never-narrower contract
/// over an explicit sample of commands.
///
/// For each command in `commands`, asserts that
/// `tool.conservative_effect().combine(tool.command_effect(&command))`
/// equals `tool.conservative_effect()` — i.e. the conservative effect is
/// at least as severe as that command's effect, so a call whose arguments
/// fail to parse (and therefore falls back to the conservative effect)
/// can never be scheduled more permissively than the command it actually
/// names. Panics with the offending command's index and both effects on
/// violation.
///
/// Every `CompositeTool` implementation must call this from a unit test
/// with one constructed value per command variant; it is the test-side
/// enforcement of a contract that cannot be checked at registration time
/// (see [`CompositeTool::conservative_effect`] for why).
pub fn assert_conservative_effect_covers_all_commands<T>(
    tool: &T,
    commands: impl IntoIterator<Item = T::Command>,
) where
    T: CompositeTool,
{
    let conservative = tool.conservative_effect();
    for (index, command) in commands.into_iter().enumerate() {
        let effect = tool.command_effect(&command);
        assert!(
            conservative.combine(effect) == conservative,
            "CompositeTool '{}': conservative_effect() = {conservative:?} is narrower than \
             command_effect = {effect:?} for the command at index {index}; a mutation could be \
             scheduled as concurrent when its arguments fail to parse. Widen \
             conservative_effect() to cover every command's effect.",
            CompositeTool::name(tool),
        );
    }
}

/// The typed invalid-arguments failure a composite returns when the
/// model's arguments do not deserialize into the command enum.
///
/// Soft (model-correctable): returned as a failed [`ToolOutput`] rather
/// than a hard [`ToolError`], so the typed payload reaches the model and
/// the `ToolResult` event with the valid command list in its detail.
fn invalid_command_output(
    tool_name: &str,
    command_field: &str,
    schema: &Value,
    parse_error: &serde_json::Error,
) -> ToolOutput {
    let valid_commands: Vec<String> = composite_commands(schema, command_field)
        .into_iter()
        .map(|command| command.value)
        .collect();
    let payload = ToolErrorPayload::new(
        ToolErrorKind::InvalidArguments,
        format!("invalid arguments for '{tool_name}': {parse_error}"),
    )
    .with_detail(serde_json::json!({
        "command_field": command_field,
        "valid_commands": valid_commands,
    }));
    ToolOutput::failure(payload)
}

#[async_trait]
impl<T: CompositeTool> Tool for T {
    fn name(&self) -> &str {
        CompositeTool::name(self)
    }

    fn description(&self) -> &str {
        CompositeTool::description(self)
    }

    fn input_schema(&self) -> Value {
        CompositeTool::input_schema(self)
    }

    fn category(&self) -> ToolCategory {
        CompositeTool::category(self)
    }

    fn usage_guidance(&self) -> Option<&str> {
        CompositeTool::usage_guidance(self)
    }

    fn post_validate_mode(&self) -> PostValidateMode {
        CompositeTool::post_validate_mode(self)
    }

    /// Whole-tool effect: the conservative effect, by definition the
    /// union of every command's effect.
    fn effect(&self) -> ToolEffect {
        self.conservative_effect()
    }

    /// Per-call effect: deserialize the typed command and classify it;
    /// arguments that don't parse get the conservative effect so a
    /// mutation is never mis-scheduled as concurrent.
    fn effect_for_args(&self, args: &Value) -> ToolEffect {
        match serde_json::from_value::<T::Command>(args.clone()) {
            Ok(command) => self.command_effect(&command),
            Err(_) => self.conservative_effect(),
        }
    }

    /// One top-level entry plus one subcommand entry per command, all
    /// derived from the command enum's schema.
    fn catalog_entries(&self) -> Vec<ToolCatalogEntry> {
        let schema = CompositeTool::input_schema(self);
        let name = CompositeTool::name(self);
        let mut entries = vec![ToolCatalogEntry::tool(
            name,
            CompositeTool::description(self),
        )];
        entries.extend(
            composite_commands(&schema, self.command_field())
                .into_iter()
                .map(|command| {
                    ToolCatalogEntry::subcommand(
                        name,
                        command.value,
                        command.description,
                        command.fields,
                    )
                }),
        );
        entries
    }

    async fn pre_validate(&self, envelope: &ToolEnvelope, ctx: &ToolContext) -> PreValidateOutcome {
        CompositeTool::pre_validate(self, envelope, ctx).await
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let command = match serde_json::from_value::<T::Command>(envelope.model_args.clone()) {
            Ok(command) => command,
            Err(parse_error) => {
                return Ok(invalid_command_output(
                    CompositeTool::name(self),
                    self.command_field(),
                    &CompositeTool::input_schema(self),
                    &parse_error,
                ));
            }
        };
        self.run(command, envelope, ctx).await
    }

    async fn post_validate(&self, output: &ToolOutput, ctx: &ToolContext) -> PostValidateOutcome {
        CompositeTool::post_validate(self, output, ctx).await
    }

    async fn on_success(&self, output: &ToolOutput, ctx: &ToolContext) {
        CompositeTool::on_success(self, output, ctx).await;
    }

    async fn register_follow_ups(
        &self,
        output: &ToolOutput,
        ctx: &ToolContext,
    ) -> Vec<FollowUpAction> {
        CompositeTool::register_follow_ups(self, output, ctx).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;
    use crate::tool::envelope::RuntimeInputs;

    /// Hand-written equivalent of a `#[derive(ToolArgs)]` internally
    /// tagged enum — the macro integration is covered by the `TaskTool`
    /// conversion; these tests pin the blanket-impl behaviour.
    #[derive(Debug, Deserialize)]
    #[serde(tag = "op", rename_all = "snake_case")]
    enum CounterCommand {
        /// Read the counter.
        Get,
        /// Add to the counter.
        Add {
            /// Amount to add.
            amount: i64,
        },
    }

    impl CounterCommand {
        fn json_schema() -> Value {
            json!({
                "oneOf": [
                    {
                        "type": "object",
                        "description": "Read the counter.",
                        "properties": { "op": { "const": "get" } },
                        "required": ["op"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "description": "Add to the counter.",
                        "properties": {
                            "op": { "const": "add" },
                            "amount": {
                                "type": "integer",
                                "description": "Amount to add."
                            }
                        },
                        "required": ["op", "amount"],
                        "additionalProperties": false
                    }
                ]
            })
        }
    }

    struct CounterTool;

    #[async_trait]
    impl CompositeTool for CounterTool {
        type Command = CounterCommand;

        fn name(&self) -> &'static str {
            "counter"
        }

        fn description(&self) -> &'static str {
            "A counter with read and mutate commands."
        }

        fn command_field(&self) -> &'static str {
            "op"
        }

        fn input_schema(&self) -> Value {
            CounterCommand::json_schema()
        }

        fn command_effect(&self, command: &CounterCommand) -> ToolEffect {
            match command {
                CounterCommand::Get => ToolEffect::ReadOnly,
                CounterCommand::Add { .. } => ToolEffect::RemoteMutation,
            }
        }

        fn conservative_effect(&self) -> ToolEffect {
            ToolEffect::RemoteMutation
        }

        async fn run(
            &self,
            command: CounterCommand,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            match command {
                CounterCommand::Get => Ok(ToolOutput::success(json!({ "value": 0 }))),
                CounterCommand::Add { amount } => {
                    Ok(ToolOutput::success(json!({ "value": amount })))
                }
            }
        }
    }

    fn envelope_for(args: Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "counter".to_string(),
            model_args: args,
            runtime_inputs: RuntimeInputs::default(),
            metadata: Value::Null,
        }
    }

    fn as_tool(tool: &CounterTool) -> &dyn Tool {
        tool
    }

    #[test]
    fn blanket_impl_exposes_tool_surface() {
        let tool = CounterTool;
        let dyn_tool = as_tool(&tool);
        assert_eq!(dyn_tool.name(), "counter");
        assert_eq!(dyn_tool.effect(), ToolEffect::RemoteMutation);
        assert_eq!(
            dyn_tool.input_schema()["oneOf"].as_array().unwrap().len(),
            2
        );
    }

    /// Contract pin: `CounterTool::conservative_effect` covers every
    /// command's effect (the doc-mandated pattern every `CompositeTool`
    /// impl applies).
    #[test]
    fn conservative_effect_covers_every_command() {
        assert_conservative_effect_covers_all_commands(
            &CounterTool,
            [CounterCommand::Get, CounterCommand::Add { amount: 1 }],
        );
    }

    #[test]
    fn effect_for_args_classifies_per_command() {
        let tool = CounterTool;
        let dyn_tool = as_tool(&tool);
        assert_eq!(
            dyn_tool.effect_for_args(&json!({ "op": "get" })),
            ToolEffect::ReadOnly,
        );
        assert_eq!(
            dyn_tool.effect_for_args(&json!({ "op": "add", "amount": 2 })),
            ToolEffect::RemoteMutation,
        );
        // Unknown command / malformed args → conservative.
        assert_eq!(
            dyn_tool.effect_for_args(&json!({ "op": "explode" })),
            ToolEffect::RemoteMutation,
        );
        assert_eq!(
            dyn_tool.effect_for_args(&json!("not an object")),
            ToolEffect::RemoteMutation,
        );
    }

    #[test]
    fn catalog_entries_include_tool_and_per_command_entries() {
        let tool = CounterTool;
        let entries = as_tool(&tool).catalog_entries();
        assert_eq!(entries.len(), 3);

        assert_eq!(entries[0].name, "counter");
        assert!(entries[0].parent_tool.is_none());

        assert_eq!(entries[1].command_value.as_deref(), Some("get"));
        assert_eq!(entries[1].parent_tool.as_deref(), Some("counter"));
        assert_eq!(entries[1].description, "Read the counter.");
        assert!(entries[1].fields.is_empty());

        assert_eq!(entries[2].command_value.as_deref(), Some("add"));
        assert_eq!(entries[2].fields.len(), 1);
        assert_eq!(entries[2].fields[0].name, "amount");
        assert_eq!(entries[2].fields[0].type_hint, "integer");
        assert!(entries[2].fields[0].required);
    }

    #[tokio::test]
    async fn execute_dispatches_typed_command() {
        let tool = CounterTool;
        let ctx = ToolContext::empty();
        let out = as_tool(&tool)
            .execute(&envelope_for(json!({ "op": "add", "amount": 5 })), &ctx)
            .await
            .unwrap();
        assert!(!out.is_error());
        assert_eq!(out.content["value"], 5);
    }

    #[tokio::test]
    async fn invalid_command_yields_typed_soft_failure_naming_valid_commands() {
        let tool = CounterTool;
        let ctx = ToolContext::empty();
        let out = as_tool(&tool)
            .execute(&envelope_for(json!({ "op": "subtract" })), &ctx)
            .await
            .expect("invalid command is a soft failure, not a hard error");
        assert!(out.is_error());
        let payload = out.error().expect("typed payload present");
        assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
        assert_eq!(payload.detail["command_field"], "op");
        assert_eq!(payload.detail["valid_commands"], json!(["get", "add"]));
        // And the same structure is model-visible in the content.
        assert_eq!(out.content["error"]["kind"], "invalid_arguments");
    }

    #[tokio::test]
    async fn missing_required_field_is_typed_invalid_arguments() {
        let tool = CounterTool;
        let ctx = ToolContext::empty();
        let out = as_tool(&tool)
            .execute(&envelope_for(json!({ "op": "add" })), &ctx)
            .await
            .unwrap();
        assert!(out.is_error());
        assert_eq!(
            out.error().unwrap().kind,
            ToolErrorKind::InvalidArguments,
            "missing `amount` must classify as invalid arguments",
        );
    }
}
