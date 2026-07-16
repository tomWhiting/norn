//! Validation for client-owned public tool-call output items.

use serde_json::{Map, Value};

use super::schema::{
    JsonShape, ValidationResult, invalid, optional_value, require_enum, require_object,
    require_string, require_strings, require_value, required_str, validate_optional_caller,
    validate_string_array, value_object,
};

pub(super) fn validate_computer_call(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "computer_call";
    require_strings(raw, &["id", "call_id"], ITEM)?;
    let checks = require_value(
        raw,
        "pending_safety_checks",
        ITEM,
        "pending_safety_checks",
        JsonShape::Array,
        false,
    )?;
    for check in checks
        .as_array()
        .ok_or_else(|| invalid(ITEM, "pending_safety_checks"))?
    {
        let check = value_object(check, ITEM, "pending_safety_checks[]")?;
        require_string(check, "id", ITEM, "pending_safety_checks[].id")?;
        for (key, field) in [
            ("code", "pending_safety_checks[].code"),
            ("message", "pending_safety_checks[].message"),
        ] {
            optional_value(check, key, ITEM, field, JsonShape::String, true)?;
        }
    }
    require_enum(
        raw,
        "status",
        ITEM,
        "status",
        &["in_progress", "completed", "incomplete"],
    )?;
    if let Some(action) = optional_value(raw, "action", ITEM, "action", JsonShape::Object, true)?
        && let Some(action) = action.as_object()
    {
        validate_computer_action(action, "action")?;
    }
    if let Some(actions) = optional_value(raw, "actions", ITEM, "actions", JsonShape::Array, true)?
        && let Some(actions) = actions.as_array()
    {
        for action in actions {
            validate_computer_action(value_object(action, ITEM, "actions[]")?, "actions[]")?;
        }
    }
    Ok(())
}

fn validate_computer_action(raw: &Map<String, Value>, prefix: &'static str) -> ValidationResult {
    const ITEM: &str = "computer_call";
    let action_type = required_str(raw, "type", ITEM, field(prefix, "type"))?;
    match action_type {
        "click" => {
            require_enum(
                raw,
                "button",
                ITEM,
                field(prefix, "button"),
                &["left", "right", "wheel", "back", "forward"],
            )?;
            require_coordinates(raw, prefix)?;
            validate_optional_keys(raw, prefix)
        }
        "double_click" | "move" => {
            require_coordinates(raw, prefix)?;
            validate_optional_keys(raw, prefix)
        }
        "drag" => {
            let path = require_value(
                raw,
                "path",
                ITEM,
                field(prefix, "path"),
                JsonShape::Array,
                false,
            )?;
            for point in path
                .as_array()
                .ok_or_else(|| invalid(ITEM, field(prefix, "path")))?
            {
                let point = value_object(point, ITEM, field(prefix, "path[]"))?;
                require_integer(point, "x", field(prefix, "path[].x"))?;
                require_integer(point, "y", field(prefix, "path[].y"))?;
            }
            validate_optional_keys(raw, prefix)
        }
        "keypress" => {
            let keys = require_value(
                raw,
                "keys",
                ITEM,
                field(prefix, "keys"),
                JsonShape::Array,
                false,
            )?;
            validate_string_array(keys, ITEM, field(prefix, "keys[]"))
        }
        "screenshot" | "wait" => Ok(()),
        "scroll" => {
            for key in ["scroll_x", "scroll_y", "x", "y"] {
                require_integer(raw, key, field(prefix, key))?;
            }
            validate_optional_keys(raw, prefix)
        }
        "type" => {
            require_string(raw, "text", ITEM, field(prefix, "text"))?;
            Ok(())
        }
        _ => Err(invalid(ITEM, field(prefix, "type"))),
    }
}

fn require_coordinates(raw: &Map<String, Value>, prefix: &'static str) -> ValidationResult {
    require_integer(raw, "x", field(prefix, "x"))?;
    require_integer(raw, "y", field(prefix, "y"))
}

fn require_integer(raw: &Map<String, Value>, key: &str, path: &'static str) -> ValidationResult {
    require_value(raw, key, "computer_call", path, JsonShape::Integer, false)?;
    Ok(())
}

fn validate_optional_keys(raw: &Map<String, Value>, prefix: &'static str) -> ValidationResult {
    if let Some(keys) = optional_value(
        raw,
        "keys",
        "computer_call",
        field(prefix, "keys"),
        JsonShape::Array,
        true,
    )? && !keys.is_null()
    {
        validate_string_array(keys, "computer_call", field(prefix, "keys[]"))?;
    }
    Ok(())
}

fn field(prefix: &'static str, suffix: &'static str) -> &'static str {
    match (prefix, suffix) {
        ("action", "type") => "action.type",
        ("action", "button") => "action.button",
        ("action", "path") => "action.path",
        ("action", "path[]") => "action.path[]",
        ("action", "path[].x") => "action.path[].x",
        ("action", "path[].y") => "action.path[].y",
        ("action", "keys") => "action.keys",
        ("action", "keys[]") => "action.keys[]",
        ("action", "scroll_x") => "action.scroll_x",
        ("action", "scroll_y") => "action.scroll_y",
        ("action", "text") => "action.text",
        ("action", "x") => "action.x",
        ("action", "y") => "action.y",
        ("actions[]", "type") => "actions[].type",
        ("actions[]", "button") => "actions[].button",
        ("actions[]", "path") => "actions[].path",
        ("actions[]", "path[]") => "actions[].path[]",
        ("actions[]", "path[].x") => "actions[].path[].x",
        ("actions[]", "path[].y") => "actions[].path[].y",
        ("actions[]", "keys") => "actions[].keys",
        ("actions[]", "keys[]") => "actions[].keys[]",
        ("actions[]", "scroll_x") => "actions[].scroll_x",
        ("actions[]", "scroll_y") => "actions[].scroll_y",
        ("actions[]", "text") => "actions[].text",
        ("actions[]", "x") => "actions[].x",
        ("actions[]", "y") => "actions[].y",
        _ => "computer action",
    }
}

pub(super) fn validate_local_shell_call(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "local_shell_call";
    require_strings(raw, &["id", "call_id"], ITEM)?;
    let action = require_object(raw, "action", ITEM, "action")?;
    require_enum(action, "type", ITEM, "action.type", &["exec"])?;
    let command = require_value(
        action,
        "command",
        ITEM,
        "action.command",
        JsonShape::Array,
        false,
    )?;
    validate_string_array(command, ITEM, "action.command[]")?;
    let env = require_object(action, "env", ITEM, "action.env")?;
    if env.values().any(|value| !value.is_string()) {
        return Err(invalid(ITEM, "action.env.*"));
    }
    optional_value(
        action,
        "timeout_ms",
        ITEM,
        "action.timeout_ms",
        JsonShape::Integer,
        true,
    )?;
    for (key, path) in [
        ("user", "action.user"),
        ("working_directory", "action.working_directory"),
    ] {
        optional_value(action, key, ITEM, path, JsonShape::String, true)?;
    }
    require_enum(
        raw,
        "status",
        ITEM,
        "status",
        &["in_progress", "completed", "incomplete"],
    )
}

pub(super) fn validate_apply_patch_call(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "apply_patch_call";
    require_strings(raw, &["id", "call_id"], ITEM)?;
    let operation = require_object(raw, "operation", ITEM, "operation")?;
    match required_str(operation, "type", ITEM, "operation.type")? {
        "create_file" | "update_file" => {
            require_strings_at_operation(operation, &["path", "diff"])?;
        }
        "delete_file" => require_strings_at_operation(operation, &["path"])?,
        _ => return Err(invalid(ITEM, "operation.type")),
    }
    require_enum(raw, "status", ITEM, "status", &["in_progress", "completed"])?;
    validate_optional_caller(raw, ITEM)?;
    optional_value(
        raw,
        "created_by",
        ITEM,
        "created_by",
        JsonShape::String,
        true,
    )?;
    Ok(())
}

fn require_strings_at_operation(
    operation: &Map<String, Value>,
    keys: &[&'static str],
) -> ValidationResult {
    for key in keys {
        let path = match *key {
            "path" => "operation.path",
            "diff" => "operation.diff",
            _ => "operation",
        };
        require_string(operation, key, "apply_patch_call", path)?;
    }
    Ok(())
}

pub(super) fn validate_mcp_approval_request(raw: &Map<String, Value>) -> ValidationResult {
    require_strings(
        raw,
        &["id", "arguments", "name", "server_label"],
        "mcp_approval_request",
    )
}
