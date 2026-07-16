//! Validation for known output items without dedicated core projections.

use serde_json::{Map, Value};

use super::schema::{
    JsonShape, ValidationResult, invalid, optional_enum, optional_value, require_enum,
    require_object, require_string, require_strings, require_value, required_str,
    validate_optional_caller, validate_string_array, value_object,
};
use super::tool_schema::validate_tools;

pub(super) fn validate_file_search(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "file_search_call";
    require_string(raw, "id", ITEM, "id")?;
    let queries = require_value(raw, "queries", ITEM, "queries", JsonShape::Array, false)?;
    validate_string_array(queries, ITEM, "queries[]")?;
    require_enum(
        raw,
        "status",
        ITEM,
        "status",
        &[
            "in_progress",
            "searching",
            "completed",
            "incomplete",
            "failed",
        ],
    )?;
    if let Some(results) = optional_value(raw, "results", ITEM, "results", JsonShape::Array, true)?
        && let Some(results) = results.as_array()
    {
        for result in results {
            let result = value_object(result, ITEM, "results[]")?;
            if let Some(attributes) = optional_value(
                result,
                "attributes",
                ITEM,
                "results[].attributes",
                JsonShape::Object,
                true,
            )? && let Some(attributes) = attributes.as_object()
                && attributes
                    .values()
                    .any(|value| !(value.is_string() || value.is_number() || value.is_boolean()))
            {
                return Err(invalid(ITEM, "results[].attributes.*"));
            }
            for (key, field) in [
                ("file_id", "results[].file_id"),
                ("filename", "results[].filename"),
                ("text", "results[].text"),
            ] {
                optional_value(result, key, ITEM, field, JsonShape::String, false)?;
            }
            optional_value(
                result,
                "score",
                ITEM,
                "results[].score",
                JsonShape::Number,
                false,
            )?;
        }
    }
    Ok(())
}

pub(super) fn validate_tool_call_output(
    raw: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    require_strings(raw, &["id", "call_id"], item_type)?;
    let output = require_value(raw, "output", item_type, "output", JsonShape::Any, false)?;
    validate_tool_output(output, item_type)?;
    require_enum(
        raw,
        "status",
        item_type,
        "status",
        &["in_progress", "completed", "incomplete"],
    )?;
    validate_optional_caller(raw, item_type)?;
    optional_value(
        raw,
        "created_by",
        item_type,
        "created_by",
        JsonShape::String,
        false,
    )?;
    Ok(())
}

pub(super) fn validate_function_call_output(raw: &Map<String, Value>) -> ValidationResult {
    validate_tool_call_output(raw, "function_call_output")
}

pub(super) fn validate_custom_tool_call_output(raw: &Map<String, Value>) -> ValidationResult {
    validate_tool_call_output(raw, "custom_tool_call_output")
}

fn validate_tool_output(output: &Value, item_type: &'static str) -> ValidationResult {
    if output.is_string() {
        return Ok(());
    }
    let parts = output
        .as_array()
        .ok_or_else(|| invalid(item_type, "output"))?;
    for part in parts {
        let part = value_object(part, item_type, "output[]")?;
        match required_str(part, "type", item_type, "output[].type")? {
            "input_text" => {
                require_string(part, "text", item_type, "output[].text")?;
                validate_prompt_cache_breakpoint(part, item_type)?;
            }
            "input_image" => {
                require_enum(
                    part,
                    "detail",
                    item_type,
                    "output[].detail",
                    &["low", "high", "auto", "original"],
                )?;
                optional_value(
                    part,
                    "file_id",
                    item_type,
                    "output[].file_id",
                    JsonShape::String,
                    true,
                )?;
                optional_value(
                    part,
                    "image_url",
                    item_type,
                    "output[].image_url",
                    JsonShape::String,
                    true,
                )?;
                validate_prompt_cache_breakpoint(part, item_type)?;
            }
            "input_file" => {
                optional_enum(
                    part,
                    "detail",
                    item_type,
                    "output[].detail",
                    &["auto", "low", "high"],
                    false,
                )?;
                for (key, field, nullable) in [
                    ("file_data", "output[].file_data", false),
                    ("file_id", "output[].file_id", true),
                    ("file_url", "output[].file_url", false),
                    ("filename", "output[].filename", false),
                ] {
                    optional_value(part, key, item_type, field, JsonShape::String, nullable)?;
                }
                validate_prompt_cache_breakpoint(part, item_type)?;
            }
            _ => return Err(invalid(item_type, "output[].type")),
        }
    }
    Ok(())
}

fn validate_prompt_cache_breakpoint(
    raw: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    let Some(value) = optional_value(
        raw,
        "prompt_cache_breakpoint",
        item_type,
        "output[].prompt_cache_breakpoint",
        JsonShape::Object,
        false,
    )?
    else {
        return Ok(());
    };
    let breakpoint = value_object(value, item_type, "output[].prompt_cache_breakpoint")?;
    require_enum(
        breakpoint,
        "mode",
        item_type,
        "output[].prompt_cache_breakpoint.mode",
        &["explicit"],
    )
}

pub(super) fn validate_computer_call_output(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "computer_call_output";
    require_strings(raw, &["id", "call_id"], ITEM)?;
    require_enum(
        raw,
        "status",
        ITEM,
        "status",
        &["completed", "incomplete", "failed", "in_progress"],
    )?;
    let output = require_object(raw, "output", ITEM, "output")?;
    require_enum(
        output,
        "type",
        ITEM,
        "output.type",
        &["computer_screenshot"],
    )?;
    for (key, field) in [
        ("file_id", "output.file_id"),
        ("image_url", "output.image_url"),
    ] {
        optional_value(output, key, ITEM, field, JsonShape::String, false)?;
    }
    if let Some(checks) = optional_value(
        raw,
        "acknowledged_safety_checks",
        ITEM,
        "acknowledged_safety_checks",
        JsonShape::Array,
        false,
    )? {
        for check in checks
            .as_array()
            .ok_or_else(|| invalid(ITEM, "acknowledged_safety_checks"))?
        {
            let check = value_object(check, ITEM, "acknowledged_safety_checks[]")?;
            require_string(check, "id", ITEM, "acknowledged_safety_checks[].id")?;
            for (key, field) in [
                ("code", "acknowledged_safety_checks[].code"),
                ("message", "acknowledged_safety_checks[].message"),
            ] {
                optional_value(check, key, ITEM, field, JsonShape::String, true)?;
            }
        }
    }
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

pub(super) fn validate_program(raw: &Map<String, Value>) -> ValidationResult {
    require_strings(raw, &["id", "call_id", "code", "fingerprint"], "program")
}

pub(super) fn validate_program_output(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "program_output";
    require_strings(raw, &["id", "call_id", "result"], ITEM)?;
    require_enum(raw, "status", ITEM, "status", &["completed", "incomplete"])
}

pub(super) fn validate_compaction(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "compaction";
    require_strings(raw, &["id", "encrypted_content"], ITEM)?;
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

pub(super) fn validate_tool_search_call(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "tool_search_call";
    require_string(raw, "id", ITEM, "id")?;
    require_value(raw, "arguments", ITEM, "arguments", JsonShape::Any, false)?;
    require_value(raw, "call_id", ITEM, "call_id", JsonShape::String, true)?;
    require_enum(raw, "execution", ITEM, "execution", &["server", "client"])?;
    require_enum(
        raw,
        "status",
        ITEM,
        "status",
        &["in_progress", "completed", "incomplete"],
    )?;
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

pub(super) fn validate_tool_search_output(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "tool_search_output";
    require_string(raw, "id", ITEM, "id")?;
    require_value(raw, "call_id", ITEM, "call_id", JsonShape::String, true)?;
    require_enum(raw, "execution", ITEM, "execution", &["server", "client"])?;
    require_enum(
        raw,
        "status",
        ITEM,
        "status",
        &["in_progress", "completed", "incomplete"],
    )?;
    validate_tools(
        require_value(raw, "tools", ITEM, "tools", JsonShape::Array, false)?,
        ITEM,
    )?;
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

pub(super) fn validate_additional_tools(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "additional_tools";
    require_string(raw, "id", ITEM, "id")?;
    require_enum(
        raw,
        "role",
        ITEM,
        "role",
        &[
            "unknown",
            "user",
            "assistant",
            "system",
            "critic",
            "discriminator",
            "developer",
            "tool",
        ],
    )?;
    validate_tools(
        require_value(raw, "tools", ITEM, "tools", JsonShape::Array, false)?,
        ITEM,
    )
}
