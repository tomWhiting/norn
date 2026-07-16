//! Public-schema validation for authoritative output items.

use serde_json::{Map, Value};

use super::{client_call_schema, core_schema, hosted_schema, known_schema};
use crate::provider::response_item::ResponseItem;

use super::super::super::ResponseReconciliationError;

pub(super) type ValidationResult = Result<(), ResponseReconciliationError>;

#[derive(Clone, Copy)]
pub(super) enum JsonShape {
    Any,
    Array,
    Boolean,
    Integer,
    Number,
    Object,
    String,
}

type ItemValidator = fn(&Map<String, Value>) -> ValidationResult;

pub(super) fn validate_authoritative_item(item: &ResponseItem) -> ValidationResult {
    let raw = item
        .raw()
        .as_object()
        .ok_or_else(|| invalid("response output item", "item"))?;
    if let Some(validator) = authoritative_validator(item.item_type()) {
        validator(raw)
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn has_authoritative_validator(item_type: &str) -> bool {
    authoritative_validator(item_type).is_some()
}

fn authoritative_validator(item_type: &str) -> Option<ItemValidator> {
    match item_type {
        "message" => Some(core_schema::validate_message),
        "file_search_call" => Some(known_schema::validate_file_search),
        "function_call" => Some(core_schema::validate_function_call),
        "function_call_output" => Some(known_schema::validate_function_call_output),
        "computer_call" => Some(client_call_schema::validate_computer_call),
        "computer_call_output" => Some(known_schema::validate_computer_call_output),
        "reasoning" => Some(core_schema::validate_reasoning),
        "program" => Some(known_schema::validate_program),
        "program_output" => Some(known_schema::validate_program_output),
        "tool_search_call" => Some(known_schema::validate_tool_search_call),
        "tool_search_output" => Some(known_schema::validate_tool_search_output),
        "additional_tools" => Some(known_schema::validate_additional_tools),
        "compaction" => Some(known_schema::validate_compaction),
        "web_search_call" => Some(core_schema::validate_web_search),
        "image_generation_call" => Some(hosted_schema::validate_image_generation),
        "code_interpreter_call" => Some(hosted_schema::validate_code_interpreter),
        "local_shell_call" => Some(client_call_schema::validate_local_shell_call),
        "local_shell_call_output" => Some(hosted_schema::validate_local_shell_output),
        "shell_call" => Some(hosted_schema::validate_shell_call),
        "shell_call_output" => Some(hosted_schema::validate_shell_output),
        "apply_patch_call" => Some(client_call_schema::validate_apply_patch_call),
        "apply_patch_call_output" => Some(hosted_schema::validate_apply_patch_output),
        "mcp_call" => Some(hosted_schema::validate_mcp_call),
        "mcp_list_tools" => Some(hosted_schema::validate_mcp_list_tools),
        "mcp_approval_request" => Some(client_call_schema::validate_mcp_approval_request),
        "mcp_approval_response" => Some(hosted_schema::validate_mcp_approval_response),
        "custom_tool_call" => Some(core_schema::validate_custom_tool_call),
        "custom_tool_call_output" => Some(known_schema::validate_custom_tool_call_output),
        _ => None,
    }
}

pub(super) fn require_strings(
    raw: &Map<String, Value>,
    fields: &[&'static str],
    item_type: &'static str,
) -> ValidationResult {
    for field in fields {
        require_string(raw, field, item_type, field)?;
    }
    Ok(())
}

pub(super) fn require_strings_at(
    raw: &Map<String, Value>,
    fields: &[(&'static str, &'static str)],
    item_type: &'static str,
) -> ValidationResult {
    for (key, path) in fields {
        require_string(raw, key, item_type, path)?;
    }
    Ok(())
}

pub(super) fn require_string<'a>(
    raw: &'a Map<String, Value>,
    key: &str,
    item_type: &'static str,
    field: &'static str,
) -> Result<&'a str, ResponseReconciliationError> {
    required_str(raw, key, item_type, field)
}

pub(super) fn required_str<'a>(
    raw: &'a Map<String, Value>,
    key: &str,
    item_type: &'static str,
    field: &'static str,
) -> Result<&'a str, ResponseReconciliationError> {
    let value = raw.get(key).ok_or_else(|| missing(item_type, field))?;
    value.as_str().ok_or_else(|| invalid(item_type, field))
}

pub(super) fn require_object<'a>(
    raw: &'a Map<String, Value>,
    key: &str,
    item_type: &'static str,
    field: &'static str,
) -> Result<&'a Map<String, Value>, ResponseReconciliationError> {
    let value = raw.get(key).ok_or_else(|| missing(item_type, field))?;
    value.as_object().ok_or_else(|| invalid(item_type, field))
}

pub(super) fn require_value<'a>(
    raw: &'a Map<String, Value>,
    key: &str,
    item_type: &'static str,
    field: &'static str,
    shape: JsonShape,
    nullable: bool,
) -> Result<&'a Value, ResponseReconciliationError> {
    let value = raw.get(key).ok_or_else(|| missing(item_type, field))?;
    validate_shape(value, item_type, field, shape, nullable)?;
    Ok(value)
}

pub(super) fn optional_value<'a>(
    raw: &'a Map<String, Value>,
    key: &str,
    item_type: &'static str,
    field: &'static str,
    shape: JsonShape,
    nullable: bool,
) -> Result<Option<&'a Value>, ResponseReconciliationError> {
    let Some(value) = raw.get(key) else {
        return Ok(None);
    };
    validate_shape(value, item_type, field, shape, nullable)?;
    Ok(Some(value))
}

pub(super) fn require_enum(
    raw: &Map<String, Value>,
    key: &str,
    item_type: &'static str,
    field: &'static str,
    allowed: &[&str],
) -> ValidationResult {
    let value = required_str(raw, key, item_type, field)?;
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(invalid(item_type, field))
    }
}

pub(super) fn optional_enum(
    raw: &Map<String, Value>,
    key: &str,
    item_type: &'static str,
    field: &'static str,
    allowed: &[&str],
    nullable: bool,
) -> ValidationResult {
    let Some(value) = raw.get(key) else {
        return Ok(());
    };
    if nullable && value.is_null() {
        return Ok(());
    }
    if value.as_str().is_some_and(|value| allowed.contains(&value)) {
        Ok(())
    } else {
        Err(invalid(item_type, field))
    }
}

pub(super) fn validate_optional_caller(
    raw: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    let Some(caller) = optional_value(raw, "caller", item_type, "caller", JsonShape::Object, true)?
    else {
        return Ok(());
    };
    let Some(caller) = caller.as_object() else {
        return Ok(());
    };
    match required_str(caller, "type", item_type, "caller.type")? {
        "direct" => Ok(()),
        "program" => {
            require_string(caller, "caller_id", item_type, "caller.caller_id")?;
            Ok(())
        }
        _ => Err(invalid(item_type, "caller.type")),
    }
}

pub(super) fn validate_string_array(
    value: &Value,
    item_type: &'static str,
    field: &'static str,
) -> ValidationResult {
    let values = value.as_array().ok_or_else(|| invalid(item_type, field))?;
    if values.iter().all(Value::is_string) {
        Ok(())
    } else {
        Err(invalid(item_type, field))
    }
}

pub(super) fn value_object<'a>(
    value: &'a Value,
    item_type: &'static str,
    field: &'static str,
) -> Result<&'a Map<String, Value>, ResponseReconciliationError> {
    value.as_object().ok_or_else(|| invalid(item_type, field))
}

fn validate_shape(
    value: &Value,
    item_type: &'static str,
    field: &'static str,
    shape: JsonShape,
    nullable: bool,
) -> ValidationResult {
    if nullable && value.is_null() {
        return Ok(());
    }
    let valid = match shape {
        JsonShape::Any => !value.is_null(),
        JsonShape::Array => value.is_array(),
        JsonShape::Boolean => value.is_boolean(),
        JsonShape::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        JsonShape::Number => value.is_number(),
        JsonShape::Object => value.is_object(),
        JsonShape::String => value.is_string(),
    };
    if valid {
        Ok(())
    } else {
        Err(invalid(item_type, field))
    }
}

pub(super) const fn invalid(
    item_type: &'static str,
    field: &'static str,
) -> ResponseReconciliationError {
    ResponseReconciliationError::InvalidAuthoritativeItemField { item_type, field }
}

const fn missing(item_type: &'static str, field: &'static str) -> ResponseReconciliationError {
    ResponseReconciliationError::MissingAuthoritativeItemField { item_type, field }
}
