//! Output schema validation and retry logic.

use crate::error::SchemaError;
use crate::provider::request::ToolDefinition;
use crate::tool::envelope::{
    ENVELOPE_DESCRIPTION_KEY, ENVELOPE_METADATA_KEY, wrap_schema_with_envelope,
};

/// Reject a user output schema whose top-level `properties` declare a
/// reserved envelope key (`tool_use_description` / `tool_use_metadata`).
///
/// Those names are claimed by the tool-call envelope on every tool the
/// model sees — including the structured-output tool — and are split off
/// the model's arguments before validation. A user schema declaring one
/// as a data field is therefore unsatisfiable when the key is required
/// (the value is stripped before its presence is checked) and silently
/// lossy when optional; both are rejected here with a typed error
/// instead. Schemas without a top-level `properties` object (map-shaped
/// or non-object schemas) pass: they cannot *declare* the reserved keys,
/// which remain claimed by the envelope at the top level — a top-level
/// data key with a reserved name in a map-shaped output is stripped
/// before validation (see `split_envelope_fields`).
///
/// Called at every boundary that accepts an output schema: the
/// `spawn_agent` argument surface (synchronous feedback to the calling
/// model) and the agent-loop entry (backstop for embedder-, fork-, and
/// script-supplied schemas).
///
/// # Errors
///
/// Returns [`SchemaError::InvalidSchema`] (boxed, matching
/// [`NornError::Schema`](crate::error::NornError::Schema)) naming the
/// colliding key and the envelope convention.
pub fn check_reserved_envelope_keys(schema: &serde_json::Value) -> Result<(), Box<SchemaError>> {
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(());
    };
    for reserved in [ENVELOPE_DESCRIPTION_KEY, ENVELOPE_METADATA_KEY] {
        if properties.contains_key(reserved) {
            return Err(Box::new(SchemaError::InvalidSchema {
                reason: format!(
                    "output schema declares top-level property '{reserved}', which is \
                     reserved by the tool-call envelope (every tool call carries \
                     '{ENVELOPE_DESCRIPTION_KEY}'/'{ENVELOPE_METADATA_KEY}' alongside its \
                     arguments and they are stripped before validation) — rename the \
                     property"
                ),
            }));
        }
    }
    Ok(())
}

/// Validates a JSON value against a JSON schema.
///
/// Returns `Ok(())` if valid, or `Err(errors)` with human-readable
/// validation error messages.
pub fn validate_against_schema(
    schema: &serde_json::Value,
    output: &serde_json::Value,
) -> Result<(), Vec<String>> {
    let validator =
        jsonschema::validator_for(schema).map_err(|e| vec![format!("invalid schema: {e}")])?;

    let errors: Vec<String> = validator
        .iter_errors(output)
        .map(|e| format!("{e}"))
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Builds the schema enforcement tool definition.
///
/// The tool accepts any output conforming to the declared schema.
/// The agent loop registers this as a function tool; the provider
/// treats it identically to any other tool — including the envelope
/// wrapping every registry tool receives, so the model attaches
/// `tool_use_description` here exactly as it does on every other call
/// (mandatory when the user schema declares a `required` array, which
/// the wrap extends; advisory otherwise — the wrap only appends to an
/// existing list). The envelope fields are split back off before
/// validation in `classify_response`; the caller's declared schema is
/// validated against the clean output only. Schemas that *declare* a
/// reserved envelope key are rejected upstream by
/// [`check_reserved_envelope_keys`] before this builder runs.
pub fn build_schema_tool(tool_name: &str, schema: &serde_json::Value) -> ToolDefinition {
    ToolDefinition {
        name: tool_name.to_string(),
        description: "Submit your final structured output. Call this tool with your result when you are done.".to_string(),
        parameters: wrap_schema_with_envelope(schema.clone()),
    }
}

/// Formats a validation error message for feeding back to the model.
pub fn format_validation_feedback(
    schema: &serde_json::Value,
    output: &serde_json::Value,
    errors: &[String],
) -> String {
    let mut feedback = String::from(
        "Schema validation failed. Your output did not conform to the required schema.\n\n",
    );
    feedback.push_str("Errors:\n");
    for err in errors {
        feedback.push_str("- ");
        feedback.push_str(err);
        feedback.push('\n');
    }
    feedback.push_str("\nExpected schema:\n");
    if let Ok(pretty) = serde_json::to_string_pretty(schema) {
        feedback.push_str(&pretty);
    }
    feedback.push_str("\n\nYour output:\n");
    if let Ok(pretty) = serde_json::to_string_pretty(output) {
        feedback.push_str(&pretty);
    }
    feedback.push_str("\n\nPlease call the tool again with corrected output.");
    feedback
}

/// Formats a nudge message when the model stops without calling the schema tool.
pub fn format_nudge(tool_name: &str, schema: &serde_json::Value) -> String {
    let mut nudge = String::new();
    nudge.push_str("You must produce structured output by calling the `");
    nudge.push_str(tool_name);
    nudge.push_str("` tool with your result.\n\n");
    nudge.push_str("Call the ");
    nudge.push_str(tool_name);
    nudge.push_str(" tool with your result conforming to this schema:\n");
    if let Ok(pretty) = serde_json::to_string_pretty(schema) {
        nudge.push_str(&pretty);
    }
    nudge
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

    fn simple_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            },
            "required": ["name", "age"]
        })
    }

    /// U2-M1 regression: a user schema declaring a reserved envelope key
    /// as a top-level property is rejected with a typed error naming the
    /// key — required-key collisions would otherwise be unsatisfiable
    /// (the value is stripped before validation) and optional ones
    /// silently lossy.
    #[test]
    fn reserved_envelope_keys_in_user_schema_are_rejected() {
        for reserved in ["tool_use_description", "tool_use_metadata"] {
            let schema = serde_json::json!({
                "type": "object",
                "properties": {
                    "answer": { "type": "string" },
                    reserved: { "type": "string" }
                },
                "required": ["answer", reserved],
                "additionalProperties": false
            });
            let err = check_reserved_envelope_keys(&schema)
                .expect_err("colliding schema must be refused");
            let SchemaError::InvalidSchema { reason } = *err else {
                panic!("expected InvalidSchema, got {err:?}");
            };
            assert!(
                reason.contains(reserved) && reason.contains("reserved"),
                "error names the colliding key and the convention: {reason}",
            );
        }
    }

    /// Schemas that cannot declare the reserved keys pass: plain object
    /// schemas without collisions, map-shaped object schemas (no
    /// `properties`), and non-object schemas.
    #[test]
    fn non_colliding_schemas_pass_reserved_key_check() {
        assert!(check_reserved_envelope_keys(&simple_schema()).is_ok());
        let map_shaped = serde_json::json!({
            "type": "object",
            "additionalProperties": { "type": "string" }
        });
        assert!(check_reserved_envelope_keys(&map_shaped).is_ok());
        let non_object = serde_json::json!({ "type": "array", "items": { "type": "string" } });
        assert!(check_reserved_envelope_keys(&non_object).is_ok());
    }

    #[test]
    fn valid_output_passes() {
        let schema = simple_schema();
        let output = serde_json::json!({"name": "Alice", "age": 30});
        assert!(validate_against_schema(&schema, &output).is_ok());
    }

    #[test]
    fn missing_required_field_fails() {
        let schema = simple_schema();
        let output = serde_json::json!({"name": "Alice"});
        let result = validate_against_schema(&schema, &output);
        assert!(result.is_err());
        let errors = result.err().expect("errors");
        assert!(!errors.is_empty());
    }

    #[test]
    fn wrong_type_fails() {
        let schema = simple_schema();
        let output = serde_json::json!({"name": "Alice", "age": "thirty"});
        let result = validate_against_schema(&schema, &output);
        assert!(result.is_err());
    }

    #[test]
    fn build_schema_tool_has_correct_name() {
        let schema = simple_schema();
        let tool = build_schema_tool("structured_output", &schema);
        assert_eq!(tool.name, "structured_output");
        let props = tool.parameters["properties"]
            .as_object()
            .expect("object schema");
        assert!(props.contains_key("name"), "user properties preserved");
        assert!(props.contains_key("age"), "user properties preserved");
        assert!(
            props.contains_key("tool_use_description"),
            "schema tool is envelope-wrapped like every other tool",
        );
        assert!(props.contains_key("tool_use_metadata"));
        let required = tool.parameters["required"]
            .as_array()
            .expect("required array");
        assert!(required.contains(&serde_json::json!("name")));
        assert!(
            required.contains(&serde_json::json!("tool_use_description")),
            "description stays mandatory on the structured-output call",
        );
    }

    #[test]
    fn nudge_contains_required_components() {
        let schema = simple_schema();
        let nudge = format_nudge("structured_output", &schema);
        assert!(nudge.contains("structured_output"));
        assert!(nudge.contains("Call the structured_output tool with your result"));
        assert!(nudge.contains("\"name\""));
        assert!(nudge.contains("\"age\""));
    }

    #[test]
    fn validation_feedback_contains_errors_and_schema() {
        let schema = simple_schema();
        let output = serde_json::json!({"name": 123});
        let feedback = format_validation_feedback(
            &schema,
            &output,
            &["'age' is a required property".to_string()],
        );
        assert!(feedback.contains("Schema validation failed"));
        assert!(feedback.contains("'age' is a required property"));
        assert!(feedback.contains("\"name\""));
    }
}
