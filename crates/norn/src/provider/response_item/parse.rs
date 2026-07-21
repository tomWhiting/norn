//! Structural parsing for completed Responses items.

use serde_json::{Map, Value};

use super::{
    KnownResponseItem, KnownResponseItemKind, OpaqueResponseItem, ResponseCompactionItem,
    ResponseContentPart, ResponseCustomToolCallItem, ResponseFunctionCallItem, ResponseItem,
    ResponseItemError, ResponseMessageItem, ResponseNullable, ResponseReasoningItem,
    ResponseWebSearchCallItem,
};

const ITEM_STATUSES: &[&str] = &["in_progress", "completed", "incomplete"];
const MESSAGE_PHASES: &[&str] = &["commentary", "final_answer"];
const WEB_SEARCH_STATUSES: &[&str] = &["in_progress", "searching", "completed", "failed"];

pub(super) fn response_item(raw: Value) -> Result<ResponseItem, ResponseItemError> {
    let item_type = {
        let object = raw
            .as_object()
            .ok_or_else(|| ResponseItemError::new("response item was not a JSON object"))?;
        required_str(object, "type", "response item missing type")?.to_owned()
    };
    match item_type.as_str() {
        "message" => message(raw),
        "reasoning" => reasoning(raw),
        "function_call" => function_call(raw),
        "custom_tool_call" => custom_tool_call(raw),
        "web_search_call" => web_search_call(raw),
        "compaction" => compaction(raw, item_type),
        _ => match KnownResponseItemKind::from_discriminator(&item_type) {
            Some(kind) => known(raw, kind),
            None => opaque(raw, item_type),
        },
    }
}

fn known(raw: Value, kind: KnownResponseItemKind) -> Result<ResponseItem, ResponseItemError> {
    let id = object(&raw)?
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(ResponseItem::Known(KnownResponseItem { raw, kind, id }))
}

fn message(raw: Value) -> Result<ResponseItem, ResponseItemError> {
    let (id, role, status, phase, content) = {
        let object = object(&raw)?;
        let values = object
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| ResponseItemError::new("response message missing content array"))?;
        (
            required_str(object, "id", "response message missing id")?.to_owned(),
            required_literal(
                object,
                "role",
                "assistant",
                "response message role was not assistant",
            )?
            .to_owned(),
            required_enum_string(
                object,
                "status",
                ITEM_STATUSES,
                "response message missing or invalid status",
            )?,
            nullable_enum_string(
                object,
                "phase",
                MESSAGE_PHASES,
                "response message phase was not commentary, final_answer, or null",
            )?,
            values
                .iter()
                .cloned()
                .map(content_part)
                .collect::<Result<Vec<_>, _>>()?,
        )
    };
    Ok(ResponseItem::Message(ResponseMessageItem {
        raw,
        id,
        role,
        status,
        phase,
        content,
    }))
}

fn content_part(raw: Value) -> Result<ResponseContentPart, ResponseItemError> {
    let object = raw
        .as_object()
        .ok_or_else(|| ResponseItemError::new("response content part was not an object"))?;
    let part_type = required_str(object, "type", "response content part missing type")?;
    match part_type {
        "output_text" => {
            let text = required_str(object, "text", "output_text part missing text")?.to_owned();
            let annotations = required_array(
                object,
                "annotations",
                "output_text part missing annotations array",
            )?;
            let logprobs = required_array(
                object,
                "logprobs",
                "output_text part missing logprobs array",
            )?;
            Ok(ResponseContentPart::OutputText {
                text,
                annotations,
                logprobs,
                raw,
            })
        }
        "refusal" => Ok(ResponseContentPart::Refusal {
            refusal: required_str(object, "refusal", "refusal part missing refusal")?.to_owned(),
            raw,
        }),
        other => Ok(ResponseContentPart::Opaque {
            part_type: other.to_owned(),
            raw,
        }),
    }
}

fn reasoning(raw: Value) -> Result<ResponseItem, ResponseItemError> {
    let (id, summary, content, encrypted_content, status) = {
        let object = object(&raw)?;
        (
            required_str(object, "id", "reasoning item missing id")?.to_owned(),
            required_array(object, "summary", "reasoning item missing summary array")?,
            optional_array(object, "content")?,
            nullable_string(object, "encrypted_content")?,
            optional_enum_string(
                object,
                "status",
                ITEM_STATUSES,
                "reasoning item status was invalid",
            )?,
        )
    };
    Ok(ResponseItem::Reasoning(ResponseReasoningItem {
        raw,
        id,
        summary,
        content,
        encrypted_content,
        status,
    }))
}

fn function_call(raw: Value) -> Result<ResponseItem, ResponseItemError> {
    let (id, call_id, name, arguments) = {
        let object = object(&raw)?;
        optional_nullable_object(object, "caller")?;
        optional_string(object, "namespace")?;
        optional_enum_string(
            object,
            "status",
            ITEM_STATUSES,
            "function_call status was invalid",
        )?;
        (
            optional_string(object, "id")?,
            required_str(object, "call_id", "function_call missing call_id")?.to_owned(),
            required_str(object, "name", "function_call missing name")?.to_owned(),
            required_str(object, "arguments", "function_call missing arguments")?.to_owned(),
        )
    };
    Ok(ResponseItem::FunctionCall(ResponseFunctionCallItem {
        raw,
        id,
        call_id,
        name,
        arguments,
    }))
}

fn custom_tool_call(raw: Value) -> Result<ResponseItem, ResponseItemError> {
    let (id, call_id, name, input) = {
        let object = object(&raw)?;
        optional_nullable_object(object, "caller")?;
        optional_string(object, "namespace")?;
        (
            optional_string(object, "id")?,
            required_str(object, "call_id", "custom_tool_call missing call_id")?.to_owned(),
            required_str(object, "name", "custom_tool_call missing name")?.to_owned(),
            required_str(object, "input", "custom_tool_call missing input")?.to_owned(),
        )
    };
    Ok(ResponseItem::CustomToolCall(ResponseCustomToolCallItem {
        raw,
        id,
        call_id,
        name,
        input,
    }))
}

fn web_search_call(raw: Value) -> Result<ResponseItem, ResponseItemError> {
    let (id, status, action) = {
        let object = object(&raw)?;
        (
            required_str(object, "id", "web_search_call missing id")?.to_owned(),
            required_enum_string(
                object,
                "status",
                WEB_SEARCH_STATUSES,
                "web_search_call missing or invalid status",
            )?,
            required_object(object, "action", "web_search_call missing action object")?,
        )
    };
    Ok(ResponseItem::WebSearchCall(ResponseWebSearchCallItem {
        raw,
        id,
        status,
        action,
    }))
}

fn compaction(raw: Value, item_type: String) -> Result<ResponseItem, ResponseItemError> {
    let (id, encrypted_content) = {
        let object = object(&raw)?;
        optional_string(object, "created_by")?;
        (
            required_str(object, "id", "compaction item missing id")?.to_owned(),
            required_nonempty_str(
                object,
                "encrypted_content",
                "compaction item missing encrypted_content",
                "compaction item encrypted_content was empty",
            )?
            .to_owned(),
        )
    };
    Ok(ResponseItem::Compaction(ResponseCompactionItem {
        raw,
        item_type,
        id,
        encrypted_content,
    }))
}

fn opaque(raw: Value, item_type: String) -> Result<ResponseItem, ResponseItemError> {
    // An unknown item's schema is unknown too. Extract a string id when one is
    // available for diagnostics, but never reject or reinterpret its raw JSON
    // because a future item gives `id` a different shape or nullability.
    let id = object(&raw)?
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(ResponseItem::Opaque(OpaqueResponseItem {
        raw,
        item_type,
        id,
    }))
}

fn object(value: &Value) -> Result<&Map<String, Value>, ResponseItemError> {
    value
        .as_object()
        .ok_or_else(|| ResponseItemError::new("response item was not a JSON object"))
}

fn required_str<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    reason: &'static str,
) -> Result<&'a str, ResponseItemError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ResponseItemError::new(reason))
}

fn required_nonempty_str<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    missing_reason: &'static str,
    empty_reason: &'static str,
) -> Result<&'a str, ResponseItemError> {
    let value = required_str(object, key, missing_reason)?;
    if value.is_empty() {
        Err(ResponseItemError::new(empty_reason))
    } else {
        Ok(value)
    }
}

fn required_literal<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    literal: &str,
    reason: &'static str,
) -> Result<&'a str, ResponseItemError> {
    let value = required_str(object, key, reason)?;
    if value == literal {
        Ok(value)
    } else {
        Err(ResponseItemError::new(reason))
    }
}

fn required_enum_string(
    object: &Map<String, Value>,
    key: &str,
    accepted: &[&str],
    reason: &'static str,
) -> Result<String, ResponseItemError> {
    let value = required_str(object, key, reason)?;
    if accepted.contains(&value) {
        Ok(value.to_owned())
    } else {
        Err(ResponseItemError::new(reason))
    }
}

fn required_array(
    object: &Map<String, Value>,
    key: &str,
    reason: &'static str,
) -> Result<Vec<Value>, ResponseItemError> {
    object
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| ResponseItemError::new(reason))
}

fn required_object(
    object: &Map<String, Value>,
    key: &str,
    reason: &'static str,
) -> Result<Value, ResponseItemError> {
    object
        .get(key)
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| ResponseItemError::new(reason))
}

fn optional_string(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, ResponseItemError> {
    object
        .get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| ResponseItemError::new("optional response item field was not text"))
        })
        .transpose()
}

fn optional_enum_string(
    object: &Map<String, Value>,
    key: &str,
    accepted: &[&str],
    reason: &'static str,
) -> Result<Option<String>, ResponseItemError> {
    let value = optional_string(object, key)?;
    if value
        .as_deref()
        .is_none_or(|candidate| accepted.contains(&candidate))
    {
        Ok(value)
    } else {
        Err(ResponseItemError::new(reason))
    }
}

fn optional_nullable_object(
    object: &Map<String, Value>,
    key: &str,
) -> Result<(), ResponseItemError> {
    match object.get(key) {
        None | Some(Value::Null | Value::Object(_)) => Ok(()),
        Some(_) => Err(ResponseItemError::new(
            "optional nullable response item field was not an object or null",
        )),
    }
}

fn optional_array(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<Value>>, ResponseItemError> {
    object
        .get(key)
        .map(|value| {
            value.as_array().cloned().ok_or_else(|| {
                ResponseItemError::new("optional response item field was not an array")
            })
        })
        .transpose()
}

fn nullable_string(
    object: &Map<String, Value>,
    key: &str,
) -> Result<ResponseNullable<String>, ResponseItemError> {
    match object.get(key) {
        None => Ok(ResponseNullable::Absent),
        Some(Value::Null) => Ok(ResponseNullable::Null),
        Some(Value::String(value)) => Ok(ResponseNullable::Value(value.clone())),
        Some(_) => Err(ResponseItemError::new(
            "nullable response item field was not text or null",
        )),
    }
}

fn nullable_enum_string(
    object: &Map<String, Value>,
    key: &str,
    accepted: &[&str],
    reason: &'static str,
) -> Result<ResponseNullable<String>, ResponseItemError> {
    let value = nullable_string(object, key)?;
    match &value {
        ResponseNullable::Absent | ResponseNullable::Null => Ok(value),
        ResponseNullable::Value(candidate) if accepted.contains(&candidate.as_str()) => Ok(value),
        ResponseNullable::Value(_) => Err(ResponseItemError::new(reason)),
    }
}
