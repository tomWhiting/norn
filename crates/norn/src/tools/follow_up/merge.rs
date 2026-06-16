//! Argument preparation for follow-up dispatch.
//!
//! A registered [`FollowUpAction`](crate::tool::follow_up::FollowUpAction)
//! carries pre-populated `args` plus a mode deciding whether those args
//! shallowly override the original call's arguments or replace them outright.

use serde_json::Value;
use thiserror::Error;

use crate::tool::follow_up::FollowUpArgsMode;

/// Failure merging follow-up overrides onto the original arguments.
///
/// Both operands must be JSON objects. A non-object operand is a malformed
/// reference (a corrupt action log entry or a tool that recorded a non-object
/// argument value) and is surfaced rather than silently coerced, so the
/// follow-up tool never dispatches a target with nonsense arguments.
#[derive(Debug, Error)]
pub enum MergeArgsError {
    /// The original call's stored arguments were not a JSON object.
    #[error("original arguments must be a JSON object, got {got}")]
    OriginalNotObject {
        /// The JSON type that was found instead of an object.
        got: &'static str,
    },
    /// The follow-up action's override arguments were not a JSON object.
    #[error("override arguments must be a JSON object, got {got}")]
    OverridesNotObject {
        /// The JSON type that was found instead of an object.
        got: &'static str,
    },
}

/// The JSON type name of `value`, for error messages.
fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Shallowly merge `overrides` onto `original`, returning a new JSON object.
///
/// Every key in `original` is preserved unless `overrides` carries the same
/// key, in which case the override value replaces it. Keys only in
/// `overrides` are added. Both operands must be JSON objects; otherwise a
/// [`MergeArgsError`] is returned.
///
/// # Errors
///
/// Returns [`MergeArgsError::OriginalNotObject`] or
/// [`MergeArgsError::OverridesNotObject`] when the respective operand is not a
/// JSON object.
pub fn merge_args(original: &Value, overrides: &Value) -> Result<Value, MergeArgsError> {
    let original_map = original
        .as_object()
        .ok_or(MergeArgsError::OriginalNotObject {
            got: json_type_name(original),
        })?;
    let override_map = overrides
        .as_object()
        .ok_or(MergeArgsError::OverridesNotObject {
            got: json_type_name(overrides),
        })?;

    let mut merged = original_map.clone();
    for (key, value) in override_map {
        merged.insert(key.clone(), value.clone());
    }
    Ok(Value::Object(merged))
}

/// Build the arguments for a follow-up's target tool according to `mode`.
///
/// [`FollowUpArgsMode::MergeOriginal`] preserves historical behavior by
/// shallowly merging `overrides` onto `original`. [`FollowUpArgsMode::Replace`]
/// returns `overrides` exactly, allowing cross-tool follow-ups to avoid
/// inheriting irrelevant fields from the source call.
///
/// # Errors
///
/// Returns [`MergeArgsError`] only when `mode` is
/// [`FollowUpArgsMode::MergeOriginal`] and either side is not a JSON object.
pub fn prepare_args(
    original: &Value,
    overrides: &Value,
    mode: FollowUpArgsMode,
) -> Result<Value, MergeArgsError> {
    match mode {
        FollowUpArgsMode::MergeOriginal => merge_args(original, overrides),
        FollowUpArgsMode::Replace => Ok(overrides.clone()),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn override_replaces_existing_key() {
        let original = json!({ "a": 1, "b": 2 });
        let overrides = json!({ "b": 3 });
        let merged = merge_args(&original, &overrides).expect("merge succeeds");
        assert_eq!(merged, json!({ "a": 1, "b": 3 }));
    }

    #[test]
    fn override_adds_new_key() {
        let original = json!({ "a": 1 });
        let overrides = json!({ "b": 2 });
        let merged = merge_args(&original, &overrides).expect("merge succeeds");
        assert_eq!(merged, json!({ "a": 1, "b": 2 }));
    }

    #[test]
    fn empty_overrides_preserve_original() {
        let original = json!({ "a": 1, "b": 2 });
        let overrides = json!({});
        let merged = merge_args(&original, &overrides).expect("merge succeeds");
        assert_eq!(merged, json!({ "a": 1, "b": 2 }));
    }

    #[test]
    fn empty_original_takes_overrides() {
        let original = json!({});
        let overrides = json!({ "a": 1 });
        let merged = merge_args(&original, &overrides).expect("merge succeeds");
        assert_eq!(merged, json!({ "a": 1 }));
    }

    #[test]
    fn merge_is_deterministic() {
        let original = json!({ "a": 1, "b": 2, "c": 3 });
        let overrides = json!({ "b": 20, "d": 40 });
        let first = merge_args(&original, &overrides).expect("merge succeeds");
        let second = merge_args(&original, &overrides).expect("merge succeeds");
        assert_eq!(first, second);
        assert_eq!(first, json!({ "a": 1, "b": 20, "c": 3, "d": 40 }));
    }

    #[test]
    fn override_replaces_nested_object_wholesale() {
        // Shallow merge: a nested object override replaces, not deep-merges.
        let original = json!({ "opts": { "x": 1, "y": 2 } });
        let overrides = json!({ "opts": { "x": 9 } });
        let merged = merge_args(&original, &overrides).expect("merge succeeds");
        assert_eq!(merged, json!({ "opts": { "x": 9 } }));
    }

    #[test]
    fn non_object_original_errors() {
        let original = json!("not an object");
        let overrides = json!({ "a": 1 });
        let err = merge_args(&original, &overrides).expect_err("must error");
        assert!(matches!(
            err,
            MergeArgsError::OriginalNotObject { got: "string" }
        ));
    }

    #[test]
    fn non_object_overrides_errors() {
        let original = json!({ "a": 1 });
        let overrides = json!([1, 2, 3]);
        let err = merge_args(&original, &overrides).expect_err("must error");
        assert!(matches!(
            err,
            MergeArgsError::OverridesNotObject { got: "array" }
        ));
    }

    #[test]
    fn null_original_errors() {
        let err = merge_args(&Value::Null, &json!({})).expect_err("must error");
        assert!(matches!(
            err,
            MergeArgsError::OriginalNotObject { got: "null" }
        ));
    }

    #[test]
    fn replace_mode_returns_overrides_exactly() {
        let original = json!({ "to": "/worker", "kind": "update", "content": "hi" });
        let overrides = json!({ "agent": "/worker" });
        let prepared =
            prepare_args(&original, &overrides, FollowUpArgsMode::Replace).expect("prepare");
        assert_eq!(prepared, json!({ "agent": "/worker" }));
    }

    #[test]
    fn replace_mode_allows_non_object_overrides() {
        let original = json!({ "a": 1 });
        let overrides = json!("freeform target input");
        let prepared =
            prepare_args(&original, &overrides, FollowUpArgsMode::Replace).expect("prepare");
        assert_eq!(prepared, json!("freeform target input"));
    }
}
