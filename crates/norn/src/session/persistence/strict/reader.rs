use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use serde_json::Value;

use crate::session::events::SessionEvent;
use crate::session::persistence::IndexCounters;

use super::line_reader::CompleteLines;
use super::{
    STRICT_SESSION_FORMAT_VERSION, SessionIndexEntry, StrictEventFile, StrictFormatHeader,
    StrictIndexFile, StrictStoreError,
};

const INDEX_FIELDS: &[&str] = &[
    "id",
    "generation",
    "name",
    "model",
    "working_dir",
    "created_at",
    "updated_at",
    "event_count",
    "status",
    "format_version",
    "total_input_tokens",
    "total_output_tokens",
    "total_cache_read_tokens",
    "rel_path",
    "parent_id",
    "fidelity",
    "origin",
];

/// Decode a strict format-2 index without skipping or repairing any row.
pub fn read_strict_index_file<R: BufRead>(
    reader: R,
    path: &Path,
) -> Result<StrictIndexFile, StrictStoreError> {
    let mut lines = CompleteLines::new(reader, path);
    let header = read_header(&mut lines, path)?;
    let mut entries = Vec::new();
    let mut first_lines = HashMap::new();
    while let Some((line, raw)) = lines.next()? {
        let value = parse_value(&raw, path, line)?;
        reject_unknown_index_field(&value, path, line)?;
        let entry: SessionIndexEntry = serde_json::from_value(value.clone())
            .map_err(|error| StrictStoreError::invalid_json(path, line, error))?;
        super::index_validation::validate_index_entry(&entry, line)?;
        require_round_trip(&value, &entry, path, line)?;
        if let Some(first_line) = first_lines.insert(entry.id.clone(), line) {
            return Err(StrictStoreError::DuplicateSessionId {
                id: entry.id,
                first_line,
                line,
            });
        }
        entries.push(entry);
    }
    super::index_validation::validate_index_relationships(&entries)?;
    Ok(StrictIndexFile { header, entries })
}

/// Decode a strict format-2 timeline without skipping or repairing any row.
pub fn read_strict_event_file<R: BufRead>(
    reader: R,
    path: &Path,
) -> Result<StrictEventFile, StrictStoreError> {
    let mut events = Vec::new();
    let (header, _, _) = visit_strict_event_file(reader, path, |event| events.push(event))?;
    Ok(StrictEventFile { header, events })
}

/// Validate a strict format-2 timeline while discarding decoded events.
///
/// Rows are decoded through the same lossless schema path as
/// [`read_strict_event_file`], but the transcript is not materialised. The
/// duplicate-event-id inventory remains resident because duplicate detection
/// is a whole-file invariant.
pub fn validate_strict_event_file<R: BufRead>(
    reader: R,
    path: &Path,
) -> Result<usize, StrictStoreError> {
    visit_strict_event_file(reader, path, drop).map(|(_, event_count, _)| event_count)
}

pub(crate) fn visit_strict_event_file<R: BufRead>(
    reader: R,
    path: &Path,
    mut visit: impl FnMut(SessionEvent),
) -> Result<(StrictFormatHeader, usize, IndexCounters), StrictStoreError> {
    let mut lines = CompleteLines::new(reader, path);
    let header = read_header(&mut lines, path)?;
    let mut first_lines = HashMap::new();
    let mut counters = IndexCounters::default();
    while let Some((line, raw)) = lines.next()? {
        let value = parse_value(&raw, path, line)?;
        let event_type = event_type(&value, path, line)?;
        if !is_known_event_type(event_type) {
            return Err(StrictStoreError::UnknownEventType {
                path: path.to_path_buf(),
                line,
                event_type: event_type.to_owned(),
            });
        }
        reject_unknown_event_field(&value, event_type, path, line)?;
        let event = deserialize_losslessly(&raw, &value, path, line)?;
        let event_id = event.base().id.to_string();
        if let Some(first_line) = first_lines.insert(event_id.clone(), line) {
            return Err(StrictStoreError::DuplicateEventId {
                path: path.to_path_buf(),
                event_id,
                first_line,
                line,
            });
        }
        counters
            .absorb(&event)
            .map_err(|overflow| StrictStoreError::IndexCounterOverflow {
                path: path.to_path_buf(),
                field: overflow.field(),
            })?;
        visit(event);
    }
    Ok((header, first_lines.len(), counters))
}

pub(super) fn read_header<R: BufRead>(
    lines: &mut CompleteLines<R>,
    path: &Path,
) -> Result<StrictFormatHeader, StrictStoreError> {
    let Some((line, raw)) = lines.next()? else {
        return Err(StrictStoreError::MissingHeader {
            path: path.to_path_buf(),
        });
    };
    let value = parse_value(&raw, path, line)?;
    let Some(object) = value.as_object() else {
        return Err(StrictStoreError::MissingHeader {
            path: path.to_path_buf(),
        });
    };
    let Some(version) = object.get("norn_session_format") else {
        return Err(StrictStoreError::MissingHeader {
            path: path.to_path_buf(),
        });
    };
    if object.len() != 1 {
        return Err(StrictStoreError::InvalidHeader {
            path: path.to_path_buf(),
            reason: "the header must contain only 'norn_session_format'".to_owned(),
        });
    }
    let version = version
        .as_u64()
        .and_then(|raw| u32::try_from(raw).ok())
        .ok_or_else(|| StrictStoreError::InvalidHeader {
            path: path.to_path_buf(),
            reason: "'norn_session_format' must be an unsigned 32-bit integer".to_owned(),
        })?;
    if version < STRICT_SESSION_FORMAT_VERSION {
        return Err(StrictStoreError::LegacyFormat {
            path: path.to_path_buf(),
            found: version,
            expected: STRICT_SESSION_FORMAT_VERSION,
        });
    }
    if version > STRICT_SESSION_FORMAT_VERSION {
        return Err(StrictStoreError::NewerFormat {
            path: path.to_path_buf(),
            found: version,
            expected: STRICT_SESSION_FORMAT_VERSION,
        });
    }
    Ok(StrictFormatHeader { version })
}

pub(super) fn parse_value(raw: &[u8], path: &Path, line: usize) -> Result<Value, StrictStoreError> {
    super::json::from_slice(raw).map_err(|error| StrictStoreError::invalid_json(path, line, error))
}

fn reject_unknown_index_field(
    value: &Value,
    path: &Path,
    line: usize,
) -> Result<(), StrictStoreError> {
    let Some(object) = value.as_object() else {
        return Err(StrictStoreError::invalid_json(
            path,
            line,
            "index rows must be JSON objects",
        ));
    };
    if let Some(field) = object
        .keys()
        .find(|field| !INDEX_FIELDS.contains(&field.as_str()))
    {
        return Err(StrictStoreError::UnknownField {
            path: path.to_path_buf(),
            line,
            field: field.clone(),
        });
    }
    Ok(())
}

pub(super) fn require_round_trip<T: serde::Serialize>(
    original: &Value,
    decoded: &T,
    path: &Path,
    line: usize,
) -> Result<(), StrictStoreError> {
    let encoded = decoded
        .serialize(serde_json::value::Serializer)
        .map_err(|error| StrictStoreError::invalid_json(path, line, error))?;
    if &encoded != original {
        return Err(StrictStoreError::NonCanonicalRow {
            path: path.to_path_buf(),
            line,
            reason: "typed decoding would change field presence or value".to_owned(),
        });
    }
    Ok(())
}

fn event_type<'a>(value: &'a Value, path: &Path, line: usize) -> Result<&'a str, StrictStoreError> {
    value
        .as_object()
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            StrictStoreError::invalid_json(path, line, "event rows require a string 'type'")
        })
}

fn is_known_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "UserMessage"
            | "AssistantMessage"
            | "SpokenResponse"
            | "ToolResult"
            | "ModelChange"
            | "ProviderEpochBoundary"
            | "Compaction"
            | "ChildBranch"
            | "ForkComplete"
            | "Label"
            | "Custom"
            | "ContextMark"
            | "RuleInjection"
    )
}

fn reject_unknown_event_field(
    value: &Value,
    event_type: &str,
    path: &Path,
    line: usize,
) -> Result<(), StrictStoreError> {
    let Some(object) = value.as_object() else {
        return Err(StrictStoreError::invalid_json(
            path,
            line,
            "event rows must be JSON objects",
        ));
    };
    let fields: &[&str] = match event_type {
        "UserMessage" | "SpokenResponse" => &["type", "base", "content"],
        "AssistantMessage" => &[
            "type",
            "base",
            "response_items",
            "content",
            "thinking",
            "reasoning",
            "tool_calls",
            "usage",
            "stop_reason",
            "response_id",
        ],
        "ToolResult" => &[
            "type",
            "base",
            "tool_call_id",
            "tool_name",
            "output",
            "spool_ref",
            "duration_ms",
        ],
        "ModelChange" => &["type", "base", "old_model", "new_model"],
        "ProviderEpochBoundary" => &["type", "base", "reason"],
        "Compaction" => &["type", "base", "summary", "replaced_event_ids"],
        "ChildBranch" => &[
            "type",
            "base",
            "parent_session_id",
            "child_session_id",
            "path_address",
            "parent_event_anchor",
            "kind",
        ],
        "ForkComplete" => &[
            "type",
            "base",
            "forked_session_id",
            "result_summary",
            "usage",
            "duration_ms",
        ],
        "Label" => &["type", "base", "label", "description"],
        "Custom" => &["type", "base", "event_type", "data"],
        "ContextMark" => &["type", "base", "mark", "target_event_id"],
        "RuleInjection" => &["type", "base", "rule_id", "delivery", "timing", "content"],
        _ => return Ok(()),
    };
    if let Some(field) = object
        .keys()
        .find(|field| !fields.contains(&field.as_str()))
    {
        return Err(StrictStoreError::UnknownField {
            path: path.to_path_buf(),
            line,
            field: field.clone(),
        });
    }
    Ok(())
}

fn deserialize_losslessly(
    raw: &[u8],
    value: &Value,
    path: &Path,
    line: usize,
) -> Result<SessionEvent, StrictStoreError> {
    let mut deserializer = serde_json::Deserializer::from_slice(raw);
    let mut ignored = Vec::new();
    let event = serde_ignored::deserialize(&mut deserializer, |field| {
        ignored.push(field.to_string());
    })
    .map_err(|error| StrictStoreError::invalid_json(path, line, error))?;
    deserializer
        .end()
        .map_err(|error| StrictStoreError::invalid_json(path, line, error))?;
    if let Some(field) = ignored.into_iter().next() {
        return Err(StrictStoreError::UnknownField {
            path: path.to_path_buf(),
            line,
            field,
        });
    }
    require_round_trip(value, &event, path, line)?;
    Ok(event)
}
