//! Per-event-schema parsing and merging for the Norn CLI (NC-004 R7).
//!
//! Two sources feed the merged
//! [`EventSchemaSet`](norn::agent_loop::event_schemas::EventSchemaSet):
//!
//! 1. A profile's `event_schemas` entry inside [`Profile::settings`]
//!    (`settings["event_schemas"]` as a JSON object keyed by the snake-
//!    case event-type name).
//! 2. CLI `--event-schema TYPE=JSON|PATH` flags, each carrying inline
//!    JSON (when the value starts with `{`) or a path to a `.json` file.
//!
//! Profile entries are loaded first; CLI entries are then merged on top
//! so the CLI wins for any duplicated type. The merger never returns
//! [`Some`] of an empty set — when no source contributes a schema, the
//! result is [`None`] and the caller leaves
//! `LoopContext::event_schemas` unset.

use std::path::Path;

use norn::agent_loop::event_schemas::{EventSchemaSet, EventType};
use norn::profile::Profile;
use serde_json::Value;

use crate::cli::BuildError;
use crate::config::parse_kv;

/// Profile settings key under which per-event schemas live. The brief
/// notes that `Profile` does not have a typed `event_schemas` field, so
/// `settings["event_schemas"]` is the integration surface.
const PROFILE_SETTINGS_KEY: &str = "event_schemas";

/// Merge per-event schemas from a profile and the CLI flag list.
///
/// Returns [`None`] when neither source contributes a schema so the
/// caller can leave `LoopContext::event_schemas` set to [`None`] and the
/// loop bypasses schema validation entirely.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when a CLI flag is malformed, a
/// `TYPE` is unknown, an inline JSON value is invalid, or a referenced
/// file cannot be read or parsed.
pub fn merge_event_schemas(
    profile: &Profile,
    cli_flags: &[String],
) -> Result<Option<EventSchemaSet>, BuildError> {
    let mut set = EventSchemaSet::new();
    let mut any_set = false;

    if let Some(Value::Object(map)) = profile.settings.get(PROFILE_SETTINGS_KEY) {
        for (key, value) in map {
            let event_type = parse_event_type(key)?;
            set.set(event_type, value.clone());
            any_set = true;
        }
    }

    for flag in cli_flags {
        let (type_str, value_str) = parse_kv(flag)?;
        let event_type = parse_event_type(&type_str)?;
        let schema = parse_inline_or_file(&value_str)?;
        set.set(event_type, schema);
        any_set = true;
    }

    if any_set { Ok(Some(set)) } else { Ok(None) }
}

/// Map a snake-case event-type name onto an [`EventType`] variant.
///
/// The DESIGN.md NC4 surface and the brief's R7 acceptance both name the
/// types in snake case (`assistant_message`, `spoken_response`, ...) —
/// libnorn's [`EventType`] enum has no `#[serde(rename_all = …)]`
/// derivation, so we map them explicitly here.
fn parse_event_type(name: &str) -> Result<EventType, BuildError> {
    match name {
        "text" => Ok(EventType::Text),
        other => Err(BuildError::Argument(format!(
            "unknown event type '{other}': expected 'text'",
        ))),
    }
}

/// Decode an inline-JSON-or-file-path value the way DESIGN.md NC4
/// requires for both `-s` / `--output-schema` and `--event-schema`.
///
/// Treats the input as inline JSON when its first non-whitespace byte is
/// `{` or `[`; otherwise treats it as a filesystem path. Failures
/// surface as [`BuildError::Argument`] so the CLI exits with code 2
/// when the user supplies a malformed schema (NC-003 R5 acceptance).
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when inline JSON does not parse,
/// when the file path cannot be read, or when the file's contents are
/// not valid JSON.
pub fn parse_inline_or_file(value: &str) -> Result<Value, BuildError> {
    let trimmed = value.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return serde_json::from_str(trimmed)
            .map_err(|err| BuildError::Argument(format!("invalid inline schema JSON: {err}")));
    }

    let path = Path::new(value);
    let _descriptor_permit = norn::resource::acquire_filesystem_operation()
        .map_err(|error| BuildError::Argument(error.to_string()))?;
    let contents = std::fs::read_to_string(path).map_err(|err| {
        BuildError::Argument(format!(
            "failed to read schema file {}: {err}",
            path.display(),
        ))
    })?;
    serde_json::from_str(&contents).map_err(|err| {
        BuildError::Argument(format!(
            "invalid JSON in schema file {}: {err}",
            path.display(),
        ))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn empty_profile() -> Profile {
        Profile::default()
    }

    fn profile_with_settings(value: Value) -> Profile {
        let mut profile = Profile::default();
        profile
            .settings
            .insert(PROFILE_SETTINGS_KEY.to_owned(), value);
        profile
    }

    #[test]
    fn no_sources_returns_none() {
        let merged = merge_event_schemas(&empty_profile(), &[]).unwrap();
        assert!(merged.is_none());
    }

    #[test]
    fn profile_only_populates_set() {
        let profile = profile_with_settings(json!({
            "text": {"type": "object"},
        }));
        let merged = merge_event_schemas(&profile, &[]).unwrap().unwrap();
        assert!(merged.has(EventType::Text));
    }

    #[test]
    fn cli_inline_json_populates_set() {
        let merged =
            merge_event_schemas(&empty_profile(), &[r#"text={"type":"object"}"#.to_owned()])
                .unwrap()
                .unwrap();
        assert!(merged.has(EventType::Text));
    }

    #[test]
    fn cli_file_path_loads_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema.json");
        std::fs::write(&path, r#"{"type":"string"}"#).unwrap();
        let flag = format!("text={}", path.display());
        let merged = merge_event_schemas(&empty_profile(), &[flag])
            .unwrap()
            .unwrap();
        let schema = merged.get(EventType::Text).unwrap();
        assert_eq!(schema, &json!({"type": "string"}));
    }

    #[test]
    fn cli_overrides_profile_for_same_type() {
        let profile = profile_with_settings(json!({
            "text": {"type": "object"},
        }));
        let flags = vec![r#"text={"type":"string"}"#.to_owned()];
        let merged = merge_event_schemas(&profile, &flags).unwrap().unwrap();
        let schema = merged.get(EventType::Text).unwrap();
        assert_eq!(schema, &json!({"type": "string"}));
    }

    #[test]
    fn unknown_event_type_returns_argument_error() {
        let err = merge_event_schemas(
            &empty_profile(),
            &[r#"made_up={"type":"object"}"#.to_owned()],
        )
        .unwrap_err();
        match err {
            BuildError::Argument(reason) => assert!(reason.contains("made_up")),
            other @ BuildError::Auth(_) => panic!("expected Argument error, got {other:?}"),
        }
    }

    #[test]
    fn invalid_inline_json_returns_argument_error() {
        let err =
            merge_event_schemas(&empty_profile(), &["text={not valid".to_owned()]).unwrap_err();
        assert!(matches!(err, BuildError::Argument(_)));
    }

    #[test]
    fn missing_file_returns_argument_error() {
        let err = merge_event_schemas(&empty_profile(), &["text=/no/such/file.json".to_owned()])
            .unwrap_err();
        assert!(matches!(err, BuildError::Argument(_)));
    }

    #[test]
    fn profile_settings_not_object_is_ignored() {
        let profile = profile_with_settings(json!("not an object"));
        let merged = merge_event_schemas(&profile, &[]).unwrap();
        assert!(
            merged.is_none(),
            "non-object settings.event_schemas must be silently ignored",
        );
    }

    #[test]
    fn parse_event_type_accepts_text() {
        parse_event_type("text").unwrap_or_else(|e| panic!("'text' failed: {e:?}"));
    }

    #[test]
    fn removed_event_types_produce_clear_error() {
        for name in [
            "assistant_message",
            "spoken_response",
            "tool_call_envelope",
            "stop_output",
            "question",
            "handoff",
            "review",
            "progress",
        ] {
            let err = parse_event_type(name);
            assert!(err.is_err(), "'{name}' should be rejected as unknown");
        }
    }
}
