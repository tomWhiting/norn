//! Validation for the larger nested tool-definition variants.

use serde_json::{Map, Value};

use super::container_tool_schema::{
    NetworkPolicyPath, validate_container_auto, validate_network_policy,
};
use super::schema::{
    JsonShape, ValidationResult, invalid, optional_enum, optional_value, require_enum,
    require_string, require_value, required_str, validate_string_array, value_object,
};
use super::tool_schema::{
    validate_allowed_callers, validate_deferred_tool_fields, validate_function,
};

pub(super) fn validate_code_interpreter(
    tool: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    let container = require_value(
        tool,
        "container",
        item_type,
        "tools[].container",
        JsonShape::Any,
        false,
    )?;
    if !container.is_string() {
        let container = value_object(container, item_type, "tools[].container")?;
        require_enum(
            container,
            "type",
            item_type,
            "tools[].container.type",
            &["auto"],
        )?;
        if let Some(files) = optional_value(
            container,
            "file_ids",
            item_type,
            "tools[].container.file_ids",
            JsonShape::Array,
            false,
        )? {
            validate_string_array(files, item_type, "tools[].container.file_ids[]")?;
        }
        optional_enum(
            container,
            "memory_limit",
            item_type,
            "tools[].container.memory_limit",
            &["1g", "4g", "16g", "64g"],
            true,
        )?;
        if let Some(policy) = optional_value(
            container,
            "network_policy",
            item_type,
            "tools[].container.network_policy",
            JsonShape::Object,
            false,
        )? {
            validate_network_policy(
                value_object(policy, item_type, "tools[].container.network_policy")?,
                item_type,
                NetworkPolicyPath::Container,
            )?;
        }
    }
    validate_allowed_callers(tool, item_type)
}

pub(super) fn validate_image_generation(
    tool: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    for (key, field, allowed) in [
        (
            "action",
            "tools[].action",
            &["generate", "edit", "auto"][..],
        ),
        (
            "background",
            "tools[].background",
            &["transparent", "opaque", "auto"],
        ),
        ("input_fidelity", "tools[].input_fidelity", &["high", "low"]),
        ("moderation", "tools[].moderation", &["auto", "low"]),
        (
            "output_format",
            "tools[].output_format",
            &["png", "webp", "jpeg"],
        ),
        (
            "quality",
            "tools[].quality",
            &["low", "medium", "high", "auto"],
        ),
    ] {
        optional_enum(
            tool,
            key,
            item_type,
            field,
            allowed,
            key == "input_fidelity",
        )?;
    }
    for (key, field) in [
        ("output_compression", "tools[].output_compression"),
        ("partial_images", "tools[].partial_images"),
    ] {
        optional_value(tool, key, item_type, field, JsonShape::Integer, false)?;
    }
    optional_value(
        tool,
        "model",
        item_type,
        "tools[].model",
        JsonShape::String,
        false,
    )?;
    optional_value(
        tool,
        "size",
        item_type,
        "tools[].size",
        JsonShape::String,
        false,
    )?;
    if let Some(mask) = optional_value(
        tool,
        "input_image_mask",
        item_type,
        "tools[].input_image_mask",
        JsonShape::Object,
        false,
    )? {
        let mask = value_object(mask, item_type, "tools[].input_image_mask")?;
        optional_value(
            mask,
            "file_id",
            item_type,
            "tools[].input_image_mask.file_id",
            JsonShape::String,
            false,
        )?;
        optional_value(
            mask,
            "image_url",
            item_type,
            "tools[].input_image_mask.image_url",
            JsonShape::String,
            false,
        )?;
    }
    Ok(())
}

pub(super) fn validate_shell(
    tool: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    validate_allowed_callers(tool, item_type)?;
    let Some(environment) = optional_value(
        tool,
        "environment",
        item_type,
        "tools[].environment",
        JsonShape::Object,
        true,
    )?
    else {
        return Ok(());
    };
    let Some(environment) = environment.as_object() else {
        return Ok(());
    };
    match required_str(environment, "type", item_type, "tools[].environment.type")? {
        "local" => Ok(()),
        "container_auto" => validate_container_auto(environment, item_type),
        "container_reference" => {
            require_string(
                environment,
                "container_id",
                item_type,
                "tools[].environment.container_id",
            )?;
            Ok(())
        }
        _ => Err(invalid(item_type, "tools[].environment.type")),
    }
}

pub(super) fn validate_custom(
    tool: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    require_string(tool, "name", item_type, "tools[].name")?;
    optional_value(
        tool,
        "description",
        item_type,
        "tools[].description",
        JsonShape::String,
        false,
    )?;
    if let Some(format) = optional_value(
        tool,
        "format",
        item_type,
        "tools[].format",
        JsonShape::Object,
        false,
    )? {
        let format = value_object(format, item_type, "tools[].format")?;
        match required_str(format, "type", item_type, "tools[].format.type")? {
            "text" => {}
            "grammar" => {
                require_enum(
                    format,
                    "syntax",
                    item_type,
                    "tools[].format.syntax",
                    &["lark", "regex"],
                )?;
                require_string(format, "definition", item_type, "tools[].format.definition")?;
            }
            _ => return Err(invalid(item_type, "tools[].format.type")),
        }
    }
    validate_deferred_tool_fields(tool, item_type)
}

pub(super) fn validate_namespace(
    tool: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    require_string(tool, "description", item_type, "tools[].description")?;
    require_string(tool, "name", item_type, "tools[].name")?;
    let nested = require_value(
        tool,
        "tools",
        item_type,
        "tools[].tools",
        JsonShape::Array,
        false,
    )?;
    for nested in nested
        .as_array()
        .ok_or_else(|| invalid(item_type, "tools[].tools"))?
    {
        let nested = value_object(nested, item_type, "tools[].tools[]")?;
        match required_str(nested, "type", item_type, "tools[].tools[].type")? {
            "function" => validate_function(nested, item_type, false)?,
            "custom" => validate_custom(nested, item_type)?,
            _ => return Err(invalid(item_type, "tools[].tools[].type")),
        }
    }
    Ok(())
}

pub(super) fn validate_tool_search(
    tool: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    optional_value(
        tool,
        "description",
        item_type,
        "tools[].description",
        JsonShape::String,
        true,
    )?;
    optional_enum(
        tool,
        "execution",
        item_type,
        "tools[].execution",
        &["server", "client"],
        false,
    )?;
    Ok(())
}

pub(super) fn validate_web_search_preview(
    tool: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    if let Some(types) = optional_value(
        tool,
        "search_content_types",
        item_type,
        "tools[].search_content_types",
        JsonShape::Array,
        false,
    )? {
        let types = types
            .as_array()
            .ok_or_else(|| invalid(item_type, "tools[].search_content_types"))?;
        if !types.iter().all(|value| {
            value
                .as_str()
                .is_some_and(|value| matches!(value, "text" | "image"))
        }) {
            return Err(invalid(item_type, "tools[].search_content_types[]"));
        }
    }
    optional_enum(
        tool,
        "search_context_size",
        item_type,
        "tools[].search_context_size",
        &["low", "medium", "high"],
        false,
    )?;
    validate_optional_location(tool, item_type, true)
}

pub(super) fn validate_optional_location(
    tool: &Map<String, Value>,
    item_type: &'static str,
    require_type: bool,
) -> ValidationResult {
    let Some(location) = optional_value(
        tool,
        "user_location",
        item_type,
        "tools[].user_location",
        JsonShape::Object,
        true,
    )?
    else {
        return Ok(());
    };
    let Some(location) = location.as_object() else {
        return Ok(());
    };
    if require_type {
        require_enum(
            location,
            "type",
            item_type,
            "tools[].user_location.type",
            &["approximate"],
        )?;
    } else {
        optional_enum(
            location,
            "type",
            item_type,
            "tools[].user_location.type",
            &["approximate"],
            false,
        )?;
    }
    for key in ["city", "country", "region", "timezone"] {
        optional_value(
            location,
            key,
            item_type,
            "tools[].user_location.field",
            JsonShape::String,
            true,
        )?;
    }
    Ok(())
}
