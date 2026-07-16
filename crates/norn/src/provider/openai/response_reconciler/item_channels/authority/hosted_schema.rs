//! Validation for hosted and shell output-item schemas.

use serde_json::{Map, Value};

use super::schema::{
    JsonShape, ValidationResult, invalid, optional_enum, optional_value, require_enum,
    require_object, require_string, require_strings, require_strings_at, require_value,
    required_str, validate_optional_caller, validate_string_array, value_object,
};

pub(super) fn validate_image_generation(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "image_generation_call";
    require_string(raw, "id", ITEM, "id")?;
    require_value(raw, "result", ITEM, "result", JsonShape::String, true)?;
    require_enum(
        raw,
        "status",
        ITEM,
        "status",
        &["in_progress", "completed", "generating", "failed"],
    )
}

pub(super) fn validate_code_interpreter(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "code_interpreter_call";
    require_strings(raw, &["id", "container_id"], ITEM)?;
    require_value(raw, "code", ITEM, "code", JsonShape::String, true)?;
    let outputs = require_value(raw, "outputs", ITEM, "outputs", JsonShape::Array, true)?;
    if let Some(outputs) = outputs.as_array() {
        for output in outputs {
            let output = value_object(output, ITEM, "outputs[]")?;
            match required_str(output, "type", ITEM, "outputs[].type")? {
                "logs" => require_string(output, "logs", ITEM, "outputs[].logs")?,
                "image" => require_string(output, "url", ITEM, "outputs[].url")?,
                _ => return Err(invalid(ITEM, "outputs[].type")),
            };
        }
    }
    require_enum(
        raw,
        "status",
        ITEM,
        "status",
        &[
            "in_progress",
            "completed",
            "incomplete",
            "interpreting",
            "failed",
        ],
    )
}

pub(super) fn validate_local_shell_output(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "local_shell_call_output";
    require_strings(raw, &["id", "output"], ITEM)?;
    optional_enum(
        raw,
        "status",
        ITEM,
        "status",
        &["in_progress", "completed", "incomplete"],
        true,
    )
}

pub(super) fn validate_shell_call(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "shell_call";
    require_strings(raw, &["id", "call_id"], ITEM)?;
    let action = require_object(raw, "action", ITEM, "action")?;
    let commands = require_value(
        action,
        "commands",
        ITEM,
        "action.commands",
        JsonShape::Array,
        false,
    )?;
    validate_string_array(commands, ITEM, "action.commands[]")?;
    require_value(
        action,
        "max_output_length",
        ITEM,
        "action.max_output_length",
        JsonShape::Integer,
        true,
    )?;
    require_value(
        action,
        "timeout_ms",
        ITEM,
        "action.timeout_ms",
        JsonShape::Integer,
        true,
    )?;
    let environment = require_value(
        raw,
        "environment",
        ITEM,
        "environment",
        JsonShape::Object,
        true,
    )?;
    if let Some(environment) = environment.as_object() {
        match required_str(environment, "type", ITEM, "environment.type")? {
            "local" => {}
            "container_reference" => {
                require_string(
                    environment,
                    "container_id",
                    ITEM,
                    "environment.container_id",
                )?;
            }
            _ => return Err(invalid(ITEM, "environment.type")),
        }
    }
    require_enum(
        raw,
        "status",
        ITEM,
        "status",
        &["in_progress", "completed", "incomplete"],
    )?;
    validate_optional_caller(raw, ITEM)?;
    optional_value(
        raw,
        "created_by",
        ITEM,
        "created_by",
        JsonShape::String,
        false,
    )?;
    Ok(())
}

pub(super) fn validate_shell_output(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "shell_call_output";
    require_strings(raw, &["id", "call_id"], ITEM)?;
    require_value(
        raw,
        "max_output_length",
        ITEM,
        "max_output_length",
        JsonShape::Integer,
        true,
    )?;
    let output = require_value(raw, "output", ITEM, "output", JsonShape::Array, false)?;
    for entry in output.as_array().ok_or_else(|| invalid(ITEM, "output"))? {
        let entry = value_object(entry, ITEM, "output[]")?;
        let outcome = require_object(entry, "outcome", ITEM, "output[].outcome")?;
        match required_str(outcome, "type", ITEM, "output[].outcome.type")? {
            "timeout" => {}
            "exit" => {
                require_value(
                    outcome,
                    "exit_code",
                    ITEM,
                    "output[].outcome.exit_code",
                    JsonShape::Integer,
                    false,
                )?;
            }
            _ => return Err(invalid(ITEM, "output[].outcome.type")),
        }
        require_strings_at(
            entry,
            &[("stderr", "output[].stderr"), ("stdout", "output[].stdout")],
            ITEM,
        )?;
        optional_value(
            entry,
            "created_by",
            ITEM,
            "output[].created_by",
            JsonShape::String,
            false,
        )?;
    }
    require_enum(
        raw,
        "status",
        ITEM,
        "status",
        &["in_progress", "completed", "incomplete"],
    )?;
    validate_optional_caller(raw, ITEM)?;
    optional_value(
        raw,
        "created_by",
        ITEM,
        "created_by",
        JsonShape::String,
        false,
    )?;
    Ok(())
}

pub(super) fn validate_apply_patch_output(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "apply_patch_call_output";
    require_strings(raw, &["id", "call_id"], ITEM)?;
    require_enum(raw, "status", ITEM, "status", &["completed", "failed"])?;
    optional_value(raw, "output", ITEM, "output", JsonShape::String, true)?;
    validate_optional_caller(raw, ITEM)?;
    optional_value(
        raw,
        "created_by",
        ITEM,
        "created_by",
        JsonShape::String,
        false,
    )?;
    Ok(())
}

pub(super) fn validate_mcp_call(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "mcp_call";
    require_strings(raw, &["id", "arguments", "name", "server_label"], ITEM)?;
    for field in ["approval_request_id", "error", "output"] {
        optional_value(raw, field, ITEM, field, JsonShape::String, true)?;
    }
    optional_enum(
        raw,
        "status",
        ITEM,
        "status",
        &[
            "in_progress",
            "completed",
            "incomplete",
            "calling",
            "failed",
        ],
        false,
    )
}

pub(super) fn validate_mcp_list_tools(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "mcp_list_tools";
    require_strings(raw, &["id", "server_label"], ITEM)?;
    let tools = require_value(raw, "tools", ITEM, "tools", JsonShape::Array, false)?;
    for tool in tools.as_array().ok_or_else(|| invalid(ITEM, "tools"))? {
        let tool = value_object(tool, ITEM, "tools[]")?;
        require_value(
            tool,
            "input_schema",
            ITEM,
            "tools[].input_schema",
            JsonShape::Any,
            false,
        )?;
        require_string(tool, "name", ITEM, "tools[].name")?;
        optional_value(
            tool,
            "annotations",
            ITEM,
            "tools[].annotations",
            JsonShape::Any,
            true,
        )?;
        optional_value(
            tool,
            "description",
            ITEM,
            "tools[].description",
            JsonShape::String,
            true,
        )?;
    }
    optional_value(raw, "error", ITEM, "error", JsonShape::String, true)?;
    Ok(())
}

pub(super) fn validate_mcp_approval_response(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "mcp_approval_response";
    require_strings(raw, &["id", "approval_request_id"], ITEM)?;
    require_value(raw, "approve", ITEM, "approve", JsonShape::Boolean, false)?;
    optional_value(raw, "reason", ITEM, "reason", JsonShape::String, true)?;
    Ok(())
}
