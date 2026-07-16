//! Nested public-schema validation for typed-core output items.

use serde_json::{Map, Value};

use super::schema::{
    JsonShape, ValidationResult, invalid, optional_value, require_enum, require_string,
    require_strings, require_strings_at, require_value, required_str, validate_optional_caller,
    validate_string_array, value_object,
};

pub(super) fn validate_function_call(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "function_call";
    require_strings(raw, &["arguments", "call_id", "name"], ITEM)?;
    optional_value(raw, "id", ITEM, "id", JsonShape::String, false)?;
    optional_value(
        raw,
        "namespace",
        ITEM,
        "namespace",
        JsonShape::String,
        false,
    )?;
    super::schema::optional_enum(
        raw,
        "status",
        ITEM,
        "status",
        &["in_progress", "completed", "incomplete"],
        false,
    )?;
    validate_optional_caller(raw, ITEM)
}

pub(super) fn validate_custom_tool_call(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "custom_tool_call";
    require_strings(raw, &["call_id", "input", "name"], ITEM)?;
    optional_value(raw, "id", ITEM, "id", JsonShape::String, false)?;
    optional_value(
        raw,
        "namespace",
        ITEM,
        "namespace",
        JsonShape::String,
        false,
    )?;
    validate_optional_caller(raw, ITEM)
}

pub(super) fn validate_message(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "message";
    let content = require_value(raw, "content", ITEM, "content", JsonShape::Array, false)?;
    for part in content.as_array().ok_or_else(|| invalid(ITEM, "content"))? {
        let part = value_object(part, ITEM, "content[]")?;
        match required_str(part, "type", ITEM, "content[].type")? {
            "output_text" => validate_output_text(part)?,
            "refusal" => {
                require_string(part, "refusal", ITEM, "content[].refusal")?;
            }
            _ => return Err(invalid(ITEM, "content[].type")),
        }
    }
    Ok(())
}

fn validate_output_text(part: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "message";
    require_string(part, "text", ITEM, "content[].text")?;
    let annotations = require_value(
        part,
        "annotations",
        ITEM,
        "content[].annotations",
        JsonShape::Array,
        false,
    )?;
    for annotation in annotations
        .as_array()
        .ok_or_else(|| invalid(ITEM, "content[].annotations"))?
    {
        validate_annotation(value_object(annotation, ITEM, "content[].annotations[]")?)?;
    }
    let logprobs = require_value(
        part,
        "logprobs",
        ITEM,
        "content[].logprobs",
        JsonShape::Array,
        false,
    )?;
    for logprob in logprobs
        .as_array()
        .ok_or_else(|| invalid(ITEM, "content[].logprobs"))?
    {
        validate_logprob(value_object(logprob, ITEM, "content[].logprobs[]")?)?;
    }
    Ok(())
}

fn validate_annotation(annotation: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "message";
    match required_str(annotation, "type", ITEM, "content[].annotations[].type")? {
        "file_citation" => {
            require_strings_at(
                annotation,
                &[
                    ("file_id", "content[].annotations[].file_id"),
                    ("filename", "content[].annotations[].filename"),
                ],
                ITEM,
            )?;
            require_value(
                annotation,
                "index",
                ITEM,
                "content[].annotations[].index",
                JsonShape::Integer,
                false,
            )?;
        }
        "url_citation" => {
            require_strings_at(
                annotation,
                &[
                    ("title", "content[].annotations[].title"),
                    ("url", "content[].annotations[].url"),
                ],
                ITEM,
            )?;
            require_annotation_indices(annotation, &["start_index", "end_index"])?;
        }
        "container_file_citation" => {
            require_strings_at(
                annotation,
                &[
                    ("container_id", "content[].annotations[].container_id"),
                    ("file_id", "content[].annotations[].file_id"),
                    ("filename", "content[].annotations[].filename"),
                ],
                ITEM,
            )?;
            require_annotation_indices(annotation, &["start_index", "end_index"])?;
        }
        "file_path" => {
            require_string(
                annotation,
                "file_id",
                ITEM,
                "content[].annotations[].file_id",
            )?;
            require_value(
                annotation,
                "index",
                ITEM,
                "content[].annotations[].index",
                JsonShape::Integer,
                false,
            )?;
        }
        _ => return Err(invalid(ITEM, "content[].annotations[].type")),
    }
    Ok(())
}

fn require_annotation_indices(
    annotation: &Map<String, Value>,
    fields: &[&'static str],
) -> ValidationResult {
    for field in fields {
        require_value(
            annotation,
            field,
            "message",
            match *field {
                "start_index" => "content[].annotations[].start_index",
                "end_index" => "content[].annotations[].end_index",
                _ => "content[].annotations[].index",
            },
            JsonShape::Integer,
            false,
        )?;
    }
    Ok(())
}

fn validate_logprob(logprob: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "message";
    require_string(logprob, "token", ITEM, "content[].logprobs[].token")?;
    validate_bytes(
        require_value(
            logprob,
            "bytes",
            ITEM,
            "content[].logprobs[].bytes",
            JsonShape::Array,
            false,
        )?,
        "content[].logprobs[].bytes[]",
    )?;
    require_value(
        logprob,
        "logprob",
        ITEM,
        "content[].logprobs[].logprob",
        JsonShape::Number,
        false,
    )?;
    let top = require_value(
        logprob,
        "top_logprobs",
        ITEM,
        "content[].logprobs[].top_logprobs",
        JsonShape::Array,
        false,
    )?;
    for candidate in top
        .as_array()
        .ok_or_else(|| invalid(ITEM, "content[].logprobs[].top_logprobs"))?
    {
        validate_top_logprob(value_object(
            candidate,
            ITEM,
            "content[].logprobs[].top_logprobs[]",
        )?)?;
    }
    Ok(())
}

fn validate_top_logprob(candidate: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "message";
    require_string(
        candidate,
        "token",
        ITEM,
        "content[].logprobs[].top_logprobs[].token",
    )?;
    validate_bytes(
        require_value(
            candidate,
            "bytes",
            ITEM,
            "content[].logprobs[].top_logprobs[].bytes",
            JsonShape::Array,
            false,
        )?,
        "content[].logprobs[].top_logprobs[].bytes[]",
    )?;
    require_value(
        candidate,
        "logprob",
        ITEM,
        "content[].logprobs[].top_logprobs[].logprob",
        JsonShape::Number,
        false,
    )?;
    Ok(())
}

fn validate_bytes(value: &Value, field: &'static str) -> ValidationResult {
    let bytes = value.as_array().ok_or_else(|| invalid("message", field))?;
    if bytes.iter().all(Value::is_number) {
        Ok(())
    } else {
        Err(invalid("message", field))
    }
}

pub(super) fn validate_reasoning(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "reasoning";
    let summary = require_value(raw, "summary", ITEM, "summary", JsonShape::Array, false)?;
    validate_reasoning_parts(summary, ITEM, "summary[]", "summary_text")?;
    if let Some(content) = optional_value(raw, "content", ITEM, "content", JsonShape::Array, false)?
    {
        validate_reasoning_parts(content, ITEM, "content[]", "reasoning_text")?;
    }
    Ok(())
}

fn validate_reasoning_parts(
    value: &Value,
    item_type: &'static str,
    field: &'static str,
    expected_type: &'static str,
) -> ValidationResult {
    let parts = value.as_array().ok_or_else(|| invalid(item_type, field))?;
    for part in parts {
        let part = value_object(part, item_type, field)?;
        let type_field = match expected_type {
            "summary_text" => "summary[].type",
            _ => "content[].type",
        };
        require_enum(part, "type", item_type, type_field, &[expected_type])?;
        let text_field = match expected_type {
            "summary_text" => "summary[].text",
            _ => "content[].text",
        };
        require_string(part, "text", item_type, text_field)?;
    }
    Ok(())
}

pub(super) fn validate_web_search(raw: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "web_search_call";
    let action = super::schema::require_object(raw, "action", ITEM, "action")?;
    match required_str(action, "type", ITEM, "action.type")? {
        "search" => validate_web_search_action(action),
        "open_page" => {
            optional_value(action, "url", ITEM, "action.url", JsonShape::String, true)?;
            Ok(())
        }
        "find_in_page" => {
            require_strings_at(
                action,
                &[("pattern", "action.pattern"), ("url", "action.url")],
                ITEM,
            )?;
            Ok(())
        }
        _ => Err(invalid(ITEM, "action.type")),
    }
}

fn validate_web_search_action(action: &Map<String, Value>) -> ValidationResult {
    const ITEM: &str = "web_search_call";
    optional_value(
        action,
        "query",
        ITEM,
        "action.query",
        JsonShape::String,
        false,
    )?;
    if let Some(queries) = optional_value(
        action,
        "queries",
        ITEM,
        "action.queries",
        JsonShape::Array,
        false,
    )? {
        validate_string_array(queries, ITEM, "action.queries[]")?;
    }
    if let Some(sources) = optional_value(
        action,
        "sources",
        ITEM,
        "action.sources",
        JsonShape::Array,
        false,
    )? {
        for source in sources
            .as_array()
            .ok_or_else(|| invalid(ITEM, "action.sources"))?
        {
            let source = value_object(source, ITEM, "action.sources[]")?;
            require_enum(source, "type", ITEM, "action.sources[].type", &["url"])?;
            require_string(source, "url", ITEM, "action.sources[].url")?;
        }
    }
    Ok(())
}
