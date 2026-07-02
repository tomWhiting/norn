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
//!   (see [`composite_commands`]).
//! * **Invalid-command handling** — arguments that don't deserialize into
//!   the command enum produce a typed
//!   [`ToolErrorKind::InvalidArguments`] failure naming the valid
//!   commands, returned as a soft (model-correctable) tool result.
//! * **Canonical-schema enforcement** — serde cannot apply
//!   `deny_unknown_fields` to internally-tagged enums, so a field the
//!   resolved command does not declare would otherwise be *silently
//!   dropped* (observed in production: `complete` called with a
//!   `metadata` field that `Complete { task_id }` cannot carry). After
//!   deserialization succeeds, the post-split arguments are validated
//!   against the matched variant of the canonical schema
//!   (`additionalProperties: false`, exact `required` arrays); violations
//!   return a typed [`ToolErrorKind::InvalidArguments`] failure naming
//!   the offending fields, the resolved command, and the command's
//!   actual field list. See `validate_command_args` for the
//!   evaluation-strategy and error-precedence rationale.
//!
//! Every [`CompositeTool`] automatically implements [`Tool`] through the
//! blanket impl; a type implements one or the other, never both.

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::catalog::{ToolCatalogEntry, ToolFieldHint, composite_commands};
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

/// Validate post-split model arguments against the canonical schema's
/// matched variant, returning the typed soft failure on violation and
/// `None` when the arguments conform.
///
/// # Why this exists
///
/// Serde cannot apply `deny_unknown_fields` to internally-tagged enums,
/// so any field the resolved command does not declare is silently
/// dropped at deserialization — the model believes it passed data the
/// tool never saw. The canonical schema derived by `ToolArgs` *does*
/// express the contract (per-variant `additionalProperties: false` and
/// exact `required` arrays), and the `OpenAI` wire projection is a
/// deliberately loosened flat merge of every command's fields (see
/// `provider::openai::schema_downlevel`), so models routinely send
/// cross-command fields. This function enforces the canonical contract
/// at dispatch for every [`CompositeTool`] via the blanket impl.
///
/// # Error precedence
///
/// This runs only *after* `serde_json::from_value::<Command>` succeeds.
/// Serde's failures (unknown command value, missing required field,
/// wrong-typed field) return first via [`invalid_command_output`]: serde
/// names the exact field and the expected Rust type for type errors,
/// which is more actionable than a generic schema-violation string, and
/// the unknown-command behaviour (listing `valid_commands`) is preserved
/// exactly. Schema validation therefore never masks a serde message — it
/// fires only on the class serde structurally cannot detect.
///
/// # Evaluation strategy
///
/// The variant is matched by the command tag and the arguments are
/// validated against that *single* variant schema rather than the root
/// `oneOf`: a `oneOf` failure reports mismatch noise across every
/// command, while the matched-variant failure names the offending fields
/// of the command the model actually invoked — a one-round-trip
/// correction. The validator is compiled per call rather than cached:
/// the blanket impl has no per-instance storage, a `static` inside a
/// generic function is shared across all monomorphizations (so it cannot
/// key per tool), and a global type-keyed cache would need a lock plus
/// poison handling. Variant schemas are a handful of flat properties
/// that `jsonschema` compiles in microseconds, on a dispatch path whose
/// calls are dominated by model round trips — per-call compilation is
/// measurably trivial and the caching complexity is unjustified.
///
/// # Null handling
///
/// Top-level properties whose value is `null` are treated as absent
/// before validation: serde deserializes an explicit `null` for an
/// `Option` field into `None` exactly as if the field were omitted, the
/// derived schema spells optionality as omission (not nullability), and
/// a `null` carries no droppable data — rejecting it would fail calls
/// the command enum itself accepts for zero data-loss protection.
/// Nested schemas are enforced exactly as the canonical variant declares
/// them, no stricter and no looser.
///
/// # Non-conforming schemas
///
/// [`CompositeTool::input_schema`] is documented as the command enum's
/// derived `json_schema()` (a root `oneOf` of tagged object variants).
/// When the schema does not have that shape there is no variant contract
/// to enforce and the call proceeds on the enum's authority alone; when
/// the shape is present but no variant matches a tag the enum accepted
/// (schema/enum drift) or the variant schema fails to compile, the drift
/// is a tool-definition bug — it is logged loudly via `tracing::warn!`
/// and the call proceeds rather than rejecting arguments the tool's own
/// command type accepted.
fn validate_command_args(
    tool_name: &str,
    command_field: &str,
    schema: &Value,
    args: &Value,
) -> Option<ToolOutput> {
    let args_object = args.as_object()?;
    let variants = schema.get("oneOf").and_then(Value::as_array)?;
    let command = args_object.get(command_field).and_then(Value::as_str)?;

    let Some(variant) = variants.iter().find(|variant| {
        variant
            .get("properties")
            .and_then(|properties| properties.get(command_field))
            .and_then(|tag| tag.get("const"))
            .and_then(Value::as_str)
            == Some(command)
    }) else {
        tracing::warn!(
            tool = tool_name,
            command,
            "composite schema has no variant for a command its enum accepted \
             (schema/enum drift); skipping canonical-schema enforcement"
        );
        return None;
    };

    let validator = match jsonschema::validator_for(variant) {
        Ok(validator) => validator,
        Err(error) => {
            tracing::warn!(
                tool = tool_name,
                command,
                %error,
                "canonical variant schema failed to compile; skipping \
                 canonical-schema enforcement"
            );
            return None;
        }
    };

    // Explicit top-level nulls deserialize as absent (`Option::None`);
    // strip them so validation matches serde's semantics (see fn docs).
    let effective_args = Value::Object(
        args_object
            .iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );

    let violations: Vec<String> = validator
        .iter_errors(&effective_args)
        .map(|error| error.to_string())
        .collect();
    if violations.is_empty() {
        return None;
    }

    Some(command_args_violation_output(&CommandArgsViolation {
        tool_name,
        command_field,
        command,
        variant,
        effective_args: &effective_args,
        violations,
    }))
}

/// Everything [`command_args_violation_output`] needs to render one
/// canonical-schema violation.
struct CommandArgsViolation<'a> {
    tool_name: &'a str,
    command_field: &'a str,
    command: &'a str,
    variant: &'a Value,
    effective_args: &'a Value,
    violations: Vec<String>,
}

/// Render a canonical-schema violation as the typed soft failure: an
/// [`ToolErrorKind::InvalidArguments`] payload whose message and detail
/// name the unknown/offending fields, the resolved command, and the
/// command's actual field list so the model can self-correct in one
/// round trip.
fn command_args_violation_output(violation: &CommandArgsViolation<'_>) -> ToolOutput {
    let known_fields: Vec<&str> = violation
        .variant
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let unknown_fields: Vec<String> = violation
        .effective_args
        .as_object()
        .map(|map| {
            map.keys()
                .filter(|key| !known_fields.contains(&key.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    let accepted_fields: Vec<ToolFieldHint> = ToolFieldHint::from_object_schema(violation.variant)
        .into_iter()
        .filter(|hint| hint.name != violation.command_field)
        .collect();

    let command = violation.command;
    let accepted_summary = if accepted_fields.is_empty() {
        format!(
            "'{command}' takes no fields other than '{}'",
            violation.command_field
        )
    } else {
        let rendered: Vec<String> = accepted_fields
            .iter()
            .map(|hint| {
                let requirement = if hint.required {
                    "required"
                } else {
                    "optional"
                };
                format!("{} ({requirement}, {})", hint.name, hint.type_hint)
            })
            .collect();
        format!("'{command}' accepts: {}", rendered.join(", "))
    };

    let problem = if unknown_fields.is_empty() {
        format!("schema violation(s): {}", violation.violations.join("; "))
    } else {
        let named: Vec<String> = unknown_fields.iter().map(|f| format!("'{f}'")).collect();
        format!(
            "unknown field(s) {} are not accepted by this command and would be silently dropped",
            named.join(", ")
        )
    };

    let payload = ToolErrorPayload::new(
        ToolErrorKind::InvalidArguments,
        format!(
            "invalid arguments for '{}': command '{command}' — {problem}. {accepted_summary}.",
            violation.tool_name
        ),
    )
    .with_detail(serde_json::json!({
        "command_field": violation.command_field,
        "command": command,
        "unknown_fields": unknown_fields,
        "violations": violation.violations,
        "accepted_fields": accepted_fields,
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
        // Serde ignores fields an internally-tagged variant does not
        // declare; enforce the canonical variant schema so nothing the
        // model sent is silently dropped (see `validate_command_args`).
        if let Some(rejection) = validate_command_args(
            CompositeTool::name(self),
            self.command_field(),
            &CompositeTool::input_schema(self),
            &envelope.model_args,
        ) {
            return Ok(rejection);
        }
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

    /// Nested settings payload for the `configure` command; serde
    /// silently ignores unknown nested fields here (no
    /// `deny_unknown_fields`), so only the canonical schema's nested
    /// `additionalProperties: false` can catch them.
    #[derive(Debug, Deserialize)]
    struct CounterSettings {
        speed: i64,
    }

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
            /// Optional note recorded with the addition.
            note: Option<String>,
        },
        /// Configure the counter.
        Configure {
            /// Counter settings.
            settings: CounterSettings,
        },
    }

    impl CounterCommand {
        fn json_schema() -> Value {
            json!({
                "type": "object",
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
                            },
                            "note": {
                                "type": "string",
                                "description": "Optional note recorded with the addition."
                            }
                        },
                        "required": ["op", "amount"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "description": "Configure the counter.",
                        "properties": {
                            "op": { "const": "configure" },
                            "settings": {
                                "type": "object",
                                "description": "Counter settings.",
                                "properties": {
                                    "speed": {
                                        "type": "integer",
                                        "description": "Tick speed."
                                    }
                                },
                                "required": ["speed"],
                                "additionalProperties": false
                            }
                        },
                        "required": ["op", "settings"],
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
                CounterCommand::Add { .. } | CounterCommand::Configure { .. } => {
                    ToolEffect::RemoteMutation
                }
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
                CounterCommand::Add { amount, note } => Ok(ToolOutput::success(
                    json!({ "value": amount, "note": note }),
                )),
                CounterCommand::Configure { settings } => Ok(ToolOutput::success(
                    json!({ "configured": true, "speed": settings.speed }),
                )),
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

    fn as_tool<T: CompositeTool>(tool: &T) -> &dyn Tool {
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
            3
        );
    }

    /// Contract pin: `CounterTool::conservative_effect` covers every
    /// command's effect (the doc-mandated pattern every `CompositeTool`
    /// impl applies).
    #[test]
    fn conservative_effect_covers_every_command() {
        assert_conservative_effect_covers_all_commands(
            &CounterTool,
            [
                CounterCommand::Get,
                CounterCommand::Add {
                    amount: 1,
                    note: None,
                },
                CounterCommand::Configure {
                    settings: CounterSettings { speed: 1 },
                },
            ],
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
        assert_eq!(entries.len(), 4);

        assert_eq!(entries[0].name, "counter");
        assert!(entries[0].parent_tool.is_none());

        assert_eq!(entries[1].command_value.as_deref(), Some("get"));
        assert_eq!(entries[1].parent_tool.as_deref(), Some("counter"));
        assert_eq!(entries[1].description, "Read the counter.");
        assert!(entries[1].fields.is_empty());

        assert_eq!(entries[2].command_value.as_deref(), Some("add"));
        assert_eq!(entries[2].fields.len(), 2);
        assert_eq!(entries[2].fields[0].name, "amount");
        assert_eq!(entries[2].fields[0].type_hint, "integer");
        assert!(entries[2].fields[0].required);
        assert_eq!(entries[2].fields[1].name, "note");
        assert!(!entries[2].fields[1].required);

        assert_eq!(entries[3].command_value.as_deref(), Some("configure"));
        assert_eq!(entries[3].fields.len(), 1);
        assert_eq!(entries[3].fields[0].name, "settings");
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
        assert_eq!(
            payload.detail["valid_commands"],
            json!(["get", "add", "configure"])
        );
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

    // -- Canonical-schema enforcement ------------------------------------

    /// The silent-drop class: a field the resolved command does not
    /// declare deserializes fine (internally-tagged enums cannot deny
    /// unknown fields) and would be dropped without a trace. The blanket
    /// impl must reject it, naming the field, the command, and the
    /// command's actual field list.
    #[tokio::test]
    async fn unknown_field_on_valid_command_is_rejected_naming_the_field() {
        let tool = CounterTool;
        let ctx = ToolContext::empty();
        let out = as_tool(&tool)
            .execute(
                &envelope_for(json!({ "op": "add", "amount": 2, "metadata": {"k": "v"} })),
                &ctx,
            )
            .await
            .expect("schema violation is a soft failure, not a hard error");
        assert!(out.is_error());
        let payload = out.error().expect("typed payload present");
        assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
        assert!(
            payload.message.contains("'metadata'"),
            "message names the unknown field: {}",
            payload.message
        );
        assert!(
            payload.message.contains("'add'"),
            "message names the resolved command: {}",
            payload.message
        );
        assert_eq!(payload.detail["command"], "add");
        assert_eq!(payload.detail["command_field"], "op");
        assert_eq!(payload.detail["unknown_fields"], json!(["metadata"]));
        let accepted = payload.detail["accepted_fields"]
            .as_array()
            .expect("accepted_fields array");
        assert!(
            accepted.iter().any(|f| f["name"] == "amount"),
            "field list lets the model self-correct: {accepted:?}"
        );
        assert!(
            !accepted.iter().any(|f| f["name"] == "op"),
            "the command tag itself is not listed as a field"
        );
        // Model-visible in the content like every typed failure.
        assert_eq!(out.content["error"]["kind"], "invalid_arguments");
    }

    /// A command with no fields of its own renders the field list as an
    /// explicit "takes no fields" hint instead of an empty list.
    #[tokio::test]
    async fn unknown_field_on_fieldless_command_says_takes_no_fields() {
        let tool = CounterTool;
        let ctx = ToolContext::empty();
        let out = as_tool(&tool)
            .execute(&envelope_for(json!({ "op": "get", "amount": 1 })), &ctx)
            .await
            .unwrap();
        assert!(out.is_error());
        let payload = out.error().expect("typed payload present");
        assert_eq!(payload.detail["unknown_fields"], json!(["amount"]));
        assert!(
            payload
                .message
                .contains("'get' takes no fields other than 'op'"),
            "message states the command takes no fields: {}",
            payload.message
        );
    }

    /// Explicit top-level nulls deserialize as absent (`Option::None`)
    /// and carry no droppable data; validation must treat them as
    /// omitted rather than reject calls the enum itself accepts.
    #[tokio::test]
    async fn explicit_null_for_optional_field_is_treated_as_absent() {
        let tool = CounterTool;
        let ctx = ToolContext::empty();
        let out = as_tool(&tool)
            .execute(
                &envelope_for(json!({ "op": "add", "amount": 3, "note": null })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["value"], 3);
    }

    /// A null-valued *unknown* field also carries no data — serde drops
    /// nothing — so it passes rather than costing a pointless round trip.
    #[tokio::test]
    async fn explicit_null_unknown_field_is_treated_as_absent() {
        let tool = CounterTool;
        let ctx = ToolContext::empty();
        let out = as_tool(&tool)
            .execute(&envelope_for(json!({ "op": "get", "stray": null })), &ctx)
            .await
            .unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
    }

    /// Nested `additionalProperties: false` is enforced exactly as the
    /// canonical variant schema declares it: serde silently drops the
    /// nested unknown field, the schema does not.
    #[tokio::test]
    async fn nested_additional_properties_enforced_as_declared() {
        let tool = CounterTool;
        let ctx = ToolContext::empty();
        let out = as_tool(&tool)
            .execute(
                &envelope_for(json!({
                    "op": "configure",
                    "settings": { "speed": 5, "turbo": true }
                })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.is_error());
        let payload = out.error().expect("typed payload present");
        assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
        assert_eq!(
            payload.detail["unknown_fields"],
            json!([]),
            "top-level fields are all known; the violation is nested"
        );
        let violations = payload.detail["violations"]
            .as_array()
            .expect("violations listed");
        assert!(
            violations
                .iter()
                .any(|v| v.as_str().is_some_and(|s| s.contains("turbo"))),
            "violation names the nested offending field: {violations:?}"
        );

        // And a conforming nested object still passes.
        let ok = as_tool(&tool)
            .execute(
                &envelope_for(json!({ "op": "configure", "settings": { "speed": 5 } })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!ok.is_error(), "{:?}", ok.content);
        assert_eq!(ok.content["speed"], 5);
    }

    /// Error precedence: a wrong-typed known field fails in serde first,
    /// whose message names the exact expectation — schema validation
    /// never runs, so it cannot mask the more actionable serde error.
    /// The serde path is recognisable by its `valid_commands` detail.
    #[tokio::test]
    async fn wrong_typed_field_yields_serde_error_not_schema_error() {
        let tool = CounterTool;
        let ctx = ToolContext::empty();
        let out = as_tool(&tool)
            .execute(
                &envelope_for(json!({ "op": "add", "amount": "five" })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.is_error());
        let payload = out.error().expect("typed payload present");
        assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
        assert!(
            payload.detail.get("valid_commands").is_some(),
            "serde-path detail shape, not the schema-violation shape: {:?}",
            payload.detail
        );
        assert!(
            payload.message.contains("invalid type"),
            "serde's type message survives: {}",
            payload.message
        );
    }

    /// A tool whose schema is missing the variant for a command its enum
    /// accepts (schema/enum drift — a tool-definition bug) warns and
    /// proceeds rather than rejecting a call the tool itself accepts.
    #[tokio::test]
    async fn schema_drift_warns_and_proceeds() {
        struct DriftTool;

        #[async_trait]
        impl CompositeTool for DriftTool {
            type Command = CounterCommand;

            fn name(&self) -> &'static str {
                "drift"
            }

            fn description(&self) -> &'static str {
                "Schema lacks the add variant."
            }

            fn command_field(&self) -> &'static str {
                "op"
            }

            fn input_schema(&self) -> Value {
                json!({
                    "type": "object",
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": { "op": { "const": "get" } },
                            "required": ["op"],
                            "additionalProperties": false
                        }
                    ]
                })
            }

            fn command_effect(&self, _command: &CounterCommand) -> ToolEffect {
                ToolEffect::RemoteMutation
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
                    CounterCommand::Add { amount, .. } => {
                        Ok(ToolOutput::success(json!({ "value": amount })))
                    }
                    CounterCommand::Get | CounterCommand::Configure { .. } => {
                        Ok(ToolOutput::success(json!({ "value": 0 })))
                    }
                }
            }
        }

        let tool = DriftTool;
        let ctx = ToolContext::empty();
        let out = as_tool(&tool)
            .execute(&envelope_for(json!({ "op": "add", "amount": 7 })), &ctx)
            .await
            .unwrap();
        assert!(
            !out.is_error(),
            "drift must not reject a call the enum accepted: {:?}",
            out.content
        );
        assert_eq!(out.content["value"], 7);
    }

    /// A schema without the documented composite `oneOf` shape offers no
    /// variant contract; enforcement skips and the enum governs alone.
    #[tokio::test]
    async fn non_composite_schema_skips_enforcement() {
        struct FlatSchemaTool;

        #[async_trait]
        impl CompositeTool for FlatSchemaTool {
            type Command = CounterCommand;

            fn name(&self) -> &'static str {
                "flat"
            }

            fn description(&self) -> &'static str {
                "Plain object schema without oneOf."
            }

            fn command_field(&self) -> &'static str {
                "op"
            }

            fn input_schema(&self) -> Value {
                json!({
                    "type": "object",
                    "properties": {
                        "op": { "type": "string" },
                        "amount": { "type": "integer" }
                    },
                    "required": ["op"]
                })
            }

            fn command_effect(&self, _command: &CounterCommand) -> ToolEffect {
                ToolEffect::RemoteMutation
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
                    CounterCommand::Add { amount, .. } => {
                        Ok(ToolOutput::success(json!({ "value": amount })))
                    }
                    CounterCommand::Get | CounterCommand::Configure { .. } => {
                        Ok(ToolOutput::success(json!({ "value": 0 })))
                    }
                }
            }
        }

        let tool = FlatSchemaTool;
        let ctx = ToolContext::empty();
        let out = as_tool(&tool)
            .execute(
                &envelope_for(json!({ "op": "add", "amount": 9, "stray": true })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["value"], 9);
    }
}
