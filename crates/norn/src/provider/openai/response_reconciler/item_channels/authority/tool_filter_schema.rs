//! Validation for recursive file-search filter definitions.

use serde_json::Value;

use super::schema::{
    JsonShape, ValidationResult, invalid, require_string, require_value, required_str, value_object,
};

pub(super) fn validate_file_search_filters(
    value: &Value,
    item_type: &'static str,
) -> ValidationResult {
    if value.is_null() {
        return Ok(());
    }

    let mut pending = vec![(value, "tools[].filters")];
    while let Some((filter, field)) = pending.pop() {
        let filter = value_object(filter, item_type, field)?;
        match required_str(filter, "type", item_type, filter_field(field, "type"))? {
            "eq" | "ne" | "gt" | "gte" | "lt" | "lte" | "in" | "nin" => {
                validate_comparison(filter, item_type, field)?;
            }
            "and" | "or" => {
                let children = require_value(
                    filter,
                    "filters",
                    item_type,
                    filter_field(field, "filters"),
                    JsonShape::Array,
                    false,
                )?;
                let children = children
                    .as_array()
                    .ok_or_else(|| invalid(item_type, filter_field(field, "filters")))?;
                pending.extend(
                    children
                        .iter()
                        .map(|child| (child, "tools[].filters.filters[]")),
                );
            }
            _ => return Err(invalid(item_type, filter_field(field, "type"))),
        }
    }
    Ok(())
}

fn validate_comparison(
    filter: &serde_json::Map<String, Value>,
    item_type: &'static str,
    field: &'static str,
) -> ValidationResult {
    require_string(filter, "key", item_type, filter_field(field, "key"))?;
    let value = require_value(
        filter,
        "value",
        item_type,
        filter_field(field, "value"),
        JsonShape::Any,
        false,
    )?;
    if comparison_value_is_valid(value) {
        Ok(())
    } else {
        Err(invalid(item_type, filter_field(field, "value")))
    }
}

fn comparison_value_is_valid(value: &Value) -> bool {
    value.is_string()
        || value.is_number()
        || value.is_boolean()
        || value.as_array().is_some_and(|values| {
            values
                .iter()
                .all(|value| value.is_string() || value.is_number())
        })
}

fn filter_field(field: &'static str, member: &'static str) -> &'static str {
    match (field, member) {
        ("tools[].filters", "type") => "tools[].filters.type",
        ("tools[].filters", "key") => "tools[].filters.key",
        ("tools[].filters", "value") => "tools[].filters.value",
        ("tools[].filters", "filters") => "tools[].filters.filters",
        (_, "type") => "tools[].filters.filters[].type",
        (_, "key") => "tools[].filters.filters[].key",
        (_, "value") => "tools[].filters.filters[].value",
        (_, "filters") => "tools[].filters.filters[].filters",
        _ => "tools[].filters",
    }
}
