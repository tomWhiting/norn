//! Required payload validation for item-scoped streaming objects.

use serde_json::Value;

use super::ResponseReconciliationError;

pub(super) fn validate_content_part(
    part: &Value,
    event_type: &'static str,
) -> Result<&'static str, ResponseReconciliationError> {
    match part.get("type").and_then(Value::as_str) {
        Some("output_text") => {
            require_string(part, "text", event_type, "part.text")?;
            require_array(part, "annotations", event_type, "part.annotations")?;
            let logprobs = require_array(part, "logprobs", event_type, "part.logprobs")?;
            for logprob in logprobs {
                validate_logprob(logprob, event_type)?;
            }
            Ok("message")
        }
        Some("refusal") => {
            require_string(part, "refusal", event_type, "part.refusal")?;
            Ok("message")
        }
        Some("reasoning_text") => {
            require_string(part, "text", event_type, "part.text")?;
            Ok("reasoning")
        }
        Some(_) | None => Err(ResponseReconciliationError::ItemScopedFamilyConflict),
    }
}

pub(super) fn validate_reasoning_summary_part(
    part: &Value,
    event_type: &'static str,
) -> Result<(), ResponseReconciliationError> {
    if part.get("type").and_then(Value::as_str) != Some("summary_text") {
        return Err(ResponseReconciliationError::ItemScopedFamilyConflict);
    }
    require_string(part, "text", event_type, "part.text")
}

fn validate_logprob(
    value: &Value,
    event_type: &'static str,
) -> Result<(), ResponseReconciliationError> {
    require_string(value, "token", event_type, "part.logprobs[].token")?;
    require_number(value, "logprob", event_type, "part.logprobs[].logprob")?;
    let bytes = require_array(value, "bytes", event_type, "part.logprobs[].bytes")?;
    if bytes.iter().any(|byte| !byte.is_number()) {
        return invalid(event_type, "part.logprobs[].bytes[]");
    }
    let top = require_array(
        value,
        "top_logprobs",
        event_type,
        "part.logprobs[].top_logprobs",
    )?;
    for candidate in top {
        require_string(
            candidate,
            "token",
            event_type,
            "part.logprobs[].top_logprobs[].token",
        )?;
        require_number(
            candidate,
            "logprob",
            event_type,
            "part.logprobs[].top_logprobs[].logprob",
        )?;
        let bytes = require_array(
            candidate,
            "bytes",
            event_type,
            "part.logprobs[].top_logprobs[].bytes",
        )?;
        if bytes.iter().any(|byte| !byte.is_number()) {
            return invalid(event_type, "part.logprobs[].top_logprobs[].bytes[]");
        }
    }
    Ok(())
}

fn require_string(
    value: &Value,
    key: &str,
    event_type: &'static str,
    field: &'static str,
) -> Result<(), ResponseReconciliationError> {
    if value.get(key).and_then(Value::as_str).is_some() {
        Ok(())
    } else {
        invalid(event_type, field)
    }
}

fn require_number(
    value: &Value,
    key: &str,
    event_type: &'static str,
    field: &'static str,
) -> Result<(), ResponseReconciliationError> {
    if value.get(key).and_then(Value::as_f64).is_some() {
        Ok(())
    } else {
        invalid(event_type, field)
    }
}

fn require_array<'a>(
    value: &'a Value,
    key: &str,
    event_type: &'static str,
    field: &'static str,
) -> Result<&'a [Value], ResponseReconciliationError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or(ResponseReconciliationError::InvalidEnvelopeField { event_type, field })
}

fn invalid<T>(
    event_type: &'static str,
    field: &'static str,
) -> Result<T, ResponseReconciliationError> {
    Err(ResponseReconciliationError::InvalidEnvelopeField { event_type, field })
}
