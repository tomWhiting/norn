//! Output schema validation and retry logic.

use crate::provider::request::ToolDefinition;

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
/// treats it identically to any other tool.
pub fn build_schema_tool(tool_name: &str, schema: &serde_json::Value) -> ToolDefinition {
    ToolDefinition {
        name: tool_name.to_string(),
        description: "Submit your final structured output. Call this tool with your result when you are done.".to_string(),
        parameters: schema.clone(),
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
        assert_eq!(tool.parameters, schema);
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
