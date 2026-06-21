//! Model-facing feedback helpers for post-validation findings.

use serde_json::Value;
use tracing::warn;

use super::failure::{ToolErrorKind, ToolErrorPayload};
use super::lifecycle::Advisory;

const REQUIRED_FEEDBACK_POLICY: &str = "Post-validation findings and convention advisories are \
required follow-up work. They are not optional notes. Fix the underlying issue properly before \
claiming the task is complete. Do not silence the finding with lint suppression, #[cfg(any())], \
ignored tests, underscore renames, or a cheap workaround. If a finding is genuinely wrong, \
document the evidence and update CONVENTIONS.toml deliberately.";

/// Append runtime post-check advisories to a model-facing tool payload.
pub(crate) fn append_advisories(content: &mut Value, advisories: &[Advisory]) {
    if advisories.is_empty() {
        return;
    }

    let entries: Vec<Value> = advisories
        .iter()
        .cloned()
        .filter_map(|advisory| match serde_json::to_value(advisory) {
            Ok(mut value) => {
                add_required_advisory_guidance(&mut value);
                Some(value)
            }
            Err(e) => {
                warn!(error = %e, "failed to serialize advisory — dropping entry");
                None
            }
        })
        .collect();

    let Some(map) = content.as_object_mut() else {
        let original = content.clone();
        *content = serde_json::json!({
            "_original": original,
            "advisory_policy": REQUIRED_FEEDBACK_POLICY,
            "advisories": entries,
        });
        return;
    };

    map.entry("advisory_policy".to_string())
        .or_insert_with(|| Value::String(REQUIRED_FEEDBACK_POLICY.to_string()));

    match map.entry("advisories".to_string()) {
        serde_json::map::Entry::Vacant(vac) => {
            vac.insert(Value::Array(entries));
        }
        serde_json::map::Entry::Occupied(mut occ) => {
            if let Value::Array(arr) = occ.get_mut() {
                arr.extend(entries);
            } else {
                occ.insert(Value::Array(entries));
            }
        }
    }
}

/// Append post-validation errors to a model-facing tool payload.
pub(crate) fn append_post_validation_errors(content: &mut Value, errors: &[String]) {
    if errors.is_empty() {
        return;
    }

    let error_values = errors
        .iter()
        .cloned()
        .map(Value::String)
        .collect::<Vec<_>>();
    let payload = ToolErrorPayload::new(
        ToolErrorKind::ValidationFailed,
        "post-validation failed; the mutation completed but the reported issues must be fixed \
         properly",
    )
    .with_detail(serde_json::json!({
        "post_validation_errors": errors,
        "guidance": REQUIRED_FEEDBACK_POLICY,
    }));

    let Some(map) = content.as_object_mut() else {
        let original = content.clone();
        *content = serde_json::json!({
            "_original": original,
            "error": payload.to_value(),
            "post_validation_errors": error_values,
            "validation_guidance": REQUIRED_FEEDBACK_POLICY,
        });
        return;
    };

    map.entry("post_validation_errors".to_string())
        .or_insert(Value::Array(error_values));
    map.entry("validation_guidance".to_string())
        .or_insert_with(|| Value::String(REQUIRED_FEEDBACK_POLICY.to_string()));
    map.entry("error".to_string())
        .or_insert_with(|| payload.to_value());
}

fn add_required_advisory_guidance(value: &mut Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    map.entry("required".to_string())
        .or_insert(Value::Bool(true));
    map.entry("guidance".to_string())
        .or_insert_with(|| Value::String(REQUIRED_FEEDBACK_POLICY.to_string()));
}
