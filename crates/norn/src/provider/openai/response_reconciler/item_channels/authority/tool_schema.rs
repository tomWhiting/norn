//! Validation for tool definitions nested in output items.

use serde_json::{Map, Value};

use super::advanced_tool_schema::{
    validate_code_interpreter, validate_custom, validate_image_generation, validate_namespace,
    validate_optional_location, validate_shell, validate_tool_search, validate_web_search_preview,
};
use super::schema::{
    JsonShape, ValidationResult, invalid, optional_enum, optional_value, require_enum,
    require_string, require_value, required_str, validate_string_array, value_object,
};
use super::tool_filter_schema::validate_file_search_filters;

pub(super) fn validate_tools(value: &Value, item_type: &'static str) -> ValidationResult {
    let tools = value
        .as_array()
        .ok_or_else(|| invalid(item_type, "tools"))?;
    for tool in tools {
        validate_tool(value_object(tool, item_type, "tools[]")?, item_type)?;
    }
    Ok(())
}

fn validate_tool(tool: &Map<String, Value>, item_type: &'static str) -> ValidationResult {
    match required_str(tool, "type", item_type, "tools[].type")? {
        "function" => validate_function(tool, item_type, true),
        "file_search" => validate_file_search(tool, item_type),
        "computer" | "programmatic_tool_calling" | "local_shell" => Ok(()),
        "computer_use_preview" => validate_computer_preview(tool, item_type),
        "web_search" | "web_search_2025_08_26" => validate_web_search(tool, item_type),
        "mcp" => validate_mcp(tool, item_type),
        "code_interpreter" => validate_code_interpreter(tool, item_type),
        "image_generation" => validate_image_generation(tool, item_type),
        "shell" => validate_shell(tool, item_type),
        "custom" => validate_custom(tool, item_type),
        "namespace" => validate_namespace(tool, item_type),
        "tool_search" => validate_tool_search(tool, item_type),
        "web_search_preview" | "web_search_preview_2025_03_11" => {
            validate_web_search_preview(tool, item_type)
        }
        "apply_patch" => validate_allowed_callers(tool, item_type),
        _ => Err(invalid(item_type, "tools[].type")),
    }
}

pub(super) fn validate_function(
    tool: &Map<String, Value>,
    item_type: &'static str,
    resource_shape: bool,
) -> ValidationResult {
    require_string(tool, "name", item_type, "tools[].name")?;
    if resource_shape {
        require_value(
            tool,
            "parameters",
            item_type,
            "tools[].parameters",
            JsonShape::Object,
            true,
        )?;
        require_value(
            tool,
            "strict",
            item_type,
            "tools[].strict",
            JsonShape::Boolean,
            true,
        )?;
    } else {
        optional_value(
            tool,
            "parameters",
            item_type,
            "tools[].parameters",
            JsonShape::Any,
            true,
        )?;
        optional_value(
            tool,
            "strict",
            item_type,
            "tools[].strict",
            JsonShape::Boolean,
            true,
        )?;
    }
    optional_value(
        tool,
        "description",
        item_type,
        "tools[].description",
        JsonShape::String,
        true,
    )?;
    optional_value(
        tool,
        "output_schema",
        item_type,
        "tools[].output_schema",
        JsonShape::Object,
        true,
    )?;
    validate_deferred_tool_fields(tool, item_type)
}

fn validate_file_search(tool: &Map<String, Value>, item_type: &'static str) -> ValidationResult {
    let stores = require_value(
        tool,
        "vector_store_ids",
        item_type,
        "tools[].vector_store_ids",
        JsonShape::Array,
        false,
    )?;
    validate_string_array(stores, item_type, "tools[].vector_store_ids[]")?;
    if let Some(filters) = optional_value(
        tool,
        "filters",
        item_type,
        "tools[].filters",
        JsonShape::Any,
        true,
    )? {
        validate_file_search_filters(filters, item_type)?;
    }
    optional_value(
        tool,
        "max_num_results",
        item_type,
        "tools[].max_num_results",
        JsonShape::Integer,
        false,
    )?;
    if let Some(ranking) = optional_value(
        tool,
        "ranking_options",
        item_type,
        "tools[].ranking_options",
        JsonShape::Object,
        false,
    )? {
        let ranking = value_object(ranking, item_type, "tools[].ranking_options")?;
        optional_enum(
            ranking,
            "ranker",
            item_type,
            "tools[].ranking_options.ranker",
            &["auto", "default-2024-11-15"],
            false,
        )?;
        optional_value(
            ranking,
            "score_threshold",
            item_type,
            "tools[].ranking_options.score_threshold",
            JsonShape::Number,
            false,
        )?;
        if let Some(hybrid) = optional_value(
            ranking,
            "hybrid_search",
            item_type,
            "tools[].ranking_options.hybrid_search",
            JsonShape::Object,
            false,
        )? {
            let hybrid = value_object(hybrid, item_type, "tools[].ranking_options.hybrid_search")?;
            require_value(
                hybrid,
                "embedding_weight",
                item_type,
                "tools[].ranking_options.hybrid_search.embedding_weight",
                JsonShape::Number,
                false,
            )?;
            require_value(
                hybrid,
                "text_weight",
                item_type,
                "tools[].ranking_options.hybrid_search.text_weight",
                JsonShape::Number,
                false,
            )?;
        }
    }
    Ok(())
}

fn validate_computer_preview(
    tool: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    require_value(
        tool,
        "display_height",
        item_type,
        "tools[].display_height",
        JsonShape::Integer,
        false,
    )?;
    require_value(
        tool,
        "display_width",
        item_type,
        "tools[].display_width",
        JsonShape::Integer,
        false,
    )?;
    require_enum(
        tool,
        "environment",
        item_type,
        "tools[].environment",
        &["windows", "mac", "linux", "ubuntu", "browser"],
    )
}

fn validate_web_search(tool: &Map<String, Value>, item_type: &'static str) -> ValidationResult {
    if let Some(filters) = optional_value(
        tool,
        "filters",
        item_type,
        "tools[].filters",
        JsonShape::Object,
        true,
    )? && let Some(filters) = filters.as_object()
        && let Some(domains) = optional_value(
            filters,
            "allowed_domains",
            item_type,
            "tools[].filters.allowed_domains",
            JsonShape::Array,
            true,
        )?
        && domains.is_array()
    {
        validate_string_array(domains, item_type, "tools[].filters.allowed_domains[]")?;
    }
    optional_enum(
        tool,
        "search_context_size",
        item_type,
        "tools[].search_context_size",
        &["low", "medium", "high"],
        false,
    )?;
    validate_optional_location(tool, item_type, false)
}

fn validate_mcp(tool: &Map<String, Value>, item_type: &'static str) -> ValidationResult {
    require_string(tool, "server_label", item_type, "tools[].server_label")?;
    validate_allowed_callers(tool, item_type)?;
    if let Some(allowed) = optional_value(
        tool,
        "allowed_tools",
        item_type,
        "tools[].allowed_tools",
        JsonShape::Any,
        true,
    )? && !allowed.is_null()
    {
        validate_string_list_or_filter(allowed, item_type, "tools[].allowed_tools")?;
    }
    for (key, field) in [
        ("authorization", "tools[].authorization"),
        ("server_description", "tools[].server_description"),
        ("server_url", "tools[].server_url"),
        ("tunnel_id", "tools[].tunnel_id"),
    ] {
        optional_value(tool, key, item_type, field, JsonShape::String, false)?;
    }
    optional_enum(
        tool,
        "connector_id",
        item_type,
        "tools[].connector_id",
        &[
            "connector_dropbox",
            "connector_gmail",
            "connector_googlecalendar",
            "connector_googledrive",
            "connector_microsoftteams",
            "connector_outlookcalendar",
            "connector_outlookemail",
            "connector_sharepoint",
        ],
        false,
    )?;
    optional_value(
        tool,
        "defer_loading",
        item_type,
        "tools[].defer_loading",
        JsonShape::Boolean,
        false,
    )?;
    if let Some(headers) = optional_value(
        tool,
        "headers",
        item_type,
        "tools[].headers",
        JsonShape::Object,
        true,
    )? && let Some(headers) = headers.as_object()
        && headers.values().any(|value| !value.is_string())
    {
        return Err(invalid(item_type, "tools[].headers.*"));
    }
    if let Some(approval) = optional_value(
        tool,
        "require_approval",
        item_type,
        "tools[].require_approval",
        JsonShape::Any,
        true,
    )? && !approval.is_null()
    {
        validate_require_approval(approval, item_type)?;
    }
    Ok(())
}

fn validate_string_list_or_filter(
    value: &Value,
    item_type: &'static str,
    field: &'static str,
) -> ValidationResult {
    if value.is_array() {
        return validate_string_array(value, item_type, field);
    }
    let filter = value_object(value, item_type, field)?;
    optional_value(
        filter,
        "read_only",
        item_type,
        "tools[].allowed_tools.read_only",
        JsonShape::Boolean,
        false,
    )?;
    if let Some(names) = optional_value(
        filter,
        "tool_names",
        item_type,
        "tools[].allowed_tools.tool_names",
        JsonShape::Array,
        false,
    )? {
        validate_string_array(names, item_type, "tools[].allowed_tools.tool_names[]")?;
    }
    Ok(())
}

fn validate_require_approval(value: &Value, item_type: &'static str) -> ValidationResult {
    if value
        .as_str()
        .is_some_and(|value| matches!(value, "always" | "never"))
    {
        return Ok(());
    }
    let approval = value_object(value, item_type, "tools[].require_approval")?;
    for key in ["always", "never"] {
        if let Some(filter) = optional_value(
            approval,
            key,
            item_type,
            "tools[].require_approval.filter",
            JsonShape::Object,
            false,
        )? {
            validate_string_list_or_filter(filter, item_type, "tools[].require_approval.filter")?;
        }
    }
    Ok(())
}

pub(super) fn validate_deferred_tool_fields(
    tool: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    validate_allowed_callers(tool, item_type)?;
    optional_value(
        tool,
        "defer_loading",
        item_type,
        "tools[].defer_loading",
        JsonShape::Boolean,
        false,
    )?;
    Ok(())
}

pub(super) fn validate_allowed_callers(
    tool: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    let Some(callers) = optional_value(
        tool,
        "allowed_callers",
        item_type,
        "tools[].allowed_callers",
        JsonShape::Array,
        true,
    )?
    else {
        return Ok(());
    };
    let Some(callers) = callers.as_array() else {
        return Ok(());
    };
    if callers.iter().all(|value| {
        value
            .as_str()
            .is_some_and(|value| matches!(value, "direct" | "programmatic"))
    }) {
        Ok(())
    } else {
        Err(invalid(item_type, "tools[].allowed_callers[]"))
    }
}
