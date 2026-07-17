use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::session::events::SessionEvent;
use crate::session::persistence::strict::ResumeFidelity;
use crate::util::PrivateRootReader;

use super::TimelineTotals;
use super::raw_lines::PhysicalLines;
use crate::session::migration::error::SessionMigrationError;
use crate::session::migration::json::{decode_known_value, parse_unique_json};
use crate::session::migration::types::LegacyClassificationReason;

pub(super) struct TimelineScan {
    pub(super) sha256: String,
    pub(super) source_format: Option<u32>,
    pub(super) fidelity: ResumeFidelity,
    pub(super) reasons: Vec<LegacyClassificationReason>,
    pub(super) totals: TimelineTotals,
}

pub(crate) fn visit_legacy_events(
    reader: &PrivateRootReader,
    path: &Path,
    indexed_format: u32,
    mut visit: impl FnMut(SessionEvent) -> Result<(), SessionMigrationError>,
) -> Result<TimelineTotals, SessionMigrationError> {
    let file = reader
        .open_file(path)
        .map_err(|error| SessionMigrationError::observation(path, error))?;
    let mut lines = PhysicalLines::new(BufReader::new(file));
    let mut saw_content = false;
    let mut seen_event_ids = HashSet::new();
    let mut totals = TimelineTotals::default();
    while let Some(line) = lines
        .next_line()
        .map_err(|error| SessionMigrationError::observation(path, error))?
    {
        if line.bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if !line.terminated {
            return Err(stage_decode_error(
                path,
                line.number,
                "timeline row is not newline-terminated",
            ));
        }
        let value = parse_unique_json(&line.bytes)
            .map_err(|error| stage_decode_error(path, line.number, &error))?;
        if !saw_content {
            saw_content = true;
            match read_legacy_header(&value) {
                HeaderDecision::Header(version) if version == indexed_format => continue,
                HeaderDecision::Header(version) => {
                    return Err(stage_decode_error(
                        path,
                        line.number,
                        &format!(
                            "timeline header format {version} disagrees with index format {indexed_format}"
                        ),
                    ));
                }
                HeaderDecision::Malformed(diagnostic) => {
                    return Err(stage_decode_error(path, line.number, &diagnostic));
                }
                HeaderDecision::Event if indexed_format == 0 => {}
                HeaderDecision::Event => {
                    return Err(stage_decode_error(
                        path,
                        line.number,
                        &format!(
                            "headerless timeline disagrees with index format {indexed_format}"
                        ),
                    ));
                }
            }
        }
        let event: SessionEvent = decode_known_value(value)
            .map_err(|error| stage_decode_error(path, line.number, &error))?;
        if !seen_event_ids.insert(event.base().id.clone()) {
            return Err(stage_decode_error(
                path,
                line.number,
                &format!("duplicate event id '{}'", event.base().id),
            ));
        }
        absorb_totals(&event, &mut totals)
            .map_err(|error| stage_decode_error(path, line.number, &error))?;
        visit(event)?;
    }
    Ok(totals)
}

pub(super) fn scan_timeline(
    reader: &PrivateRootReader,
    path: &Path,
    indexed_format: u32,
) -> Result<TimelineScan, SessionMigrationError> {
    let file = reader
        .open_file(path)
        .map_err(|error| SessionMigrationError::observation(path, error))?;
    let mut lines = PhysicalLines::new(BufReader::new(file));
    let mut hasher = Sha256::new();
    let mut source_format = None;
    let mut saw_content = false;
    let mut saw_event = false;
    let mut seen_event_ids = HashSet::new();
    let mut fidelity = ResumeFidelity::Canonical;
    let mut reasons = Vec::new();
    let mut totals = TimelineTotals::default();
    let mut invalid = false;

    while let Some(line) = lines
        .next_line()
        .map_err(|error| SessionMigrationError::observation(path, error))?
    {
        hasher.update(&line.bytes);
        if line.terminated {
            hasher.update(b"\n");
        }
        if invalid || line.bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if !line.terminated {
            reasons.push(invalid_timeline_reason(
                line.number,
                "timeline row is not newline-terminated".to_owned(),
            ));
            invalid = true;
            continue;
        }
        let value = match parse_unique_json(&line.bytes) {
            Ok(value) => value,
            Err(diagnostic) => {
                reasons.push(invalid_timeline_reason(line.number, diagnostic));
                invalid = true;
                continue;
            }
        };
        if !saw_content {
            saw_content = true;
            match read_legacy_header(&value) {
                HeaderDecision::Header(version) => {
                    source_format = Some(version);
                    if version != indexed_format {
                        reasons.push(invalid_timeline_reason(
                            line.number,
                            format!(
                                "timeline header format {version} disagrees with index format {indexed_format}"
                            ),
                        ));
                        invalid = true;
                    }
                    continue;
                }
                HeaderDecision::Malformed(diagnostic) => {
                    reasons.push(invalid_timeline_reason(line.number, diagnostic));
                    invalid = true;
                    continue;
                }
                HeaderDecision::Event => {
                    source_format = Some(0);
                    reasons.push(LegacyClassificationReason::HeaderlessTimeline);
                    fidelity = ResumeFidelity::FreshEpochProjection;
                    if indexed_format != 0 {
                        reasons.push(invalid_timeline_reason(
                            line.number,
                            format!(
                                "headerless timeline disagrees with index format {indexed_format}"
                            ),
                        ));
                        invalid = true;
                        continue;
                    }
                }
            }
        }
        let event: SessionEvent = match decode_known_value(value.clone()) {
            Ok(event) => event,
            Err(diagnostic) => {
                reasons.push(invalid_timeline_reason(line.number, diagnostic));
                invalid = true;
                continue;
            }
        };
        saw_event = true;
        if !seen_event_ids.insert(event.base().id.clone()) {
            reasons.push(invalid_timeline_reason(
                line.number,
                format!("duplicate event id '{}'", event.base().id),
            ));
            invalid = true;
            continue;
        }
        if matches!(&event, SessionEvent::ProviderEpochBoundary { .. }) {
            reasons.push(LegacyClassificationReason::SpoofedProviderEpochBoundary);
            invalid = true;
            continue;
        }
        if flattened_assistant_turn(&value) {
            if !reasons.contains(&LegacyClassificationReason::FlattenedAssistantTurn) {
                reasons.push(LegacyClassificationReason::FlattenedAssistantTurn);
            }
            fidelity = ResumeFidelity::FreshEpochProjection;
        }
        if let Err(diagnostic) = absorb_totals(&event, &mut totals) {
            reasons.push(invalid_timeline_reason(line.number, diagnostic));
            invalid = true;
        }
    }

    if !saw_content {
        source_format = Some(indexed_format);
        reasons.push(LegacyClassificationReason::HeaderlessTimeline);
        fidelity = ResumeFidelity::FreshEpochProjection;
    }
    if !saw_event {
        reasons.push(LegacyClassificationReason::EmptyTimeline);
        fidelity = ResumeFidelity::FreshEpochProjection;
    }
    if invalid {
        fidelity = ResumeFidelity::InspectOnly;
        totals = TimelineTotals::default();
    }
    Ok(TimelineScan {
        sha256: format!("{:x}", hasher.finalize()),
        source_format,
        fidelity,
        reasons,
        totals,
    })
}

pub(crate) fn hash_file(
    reader: &PrivateRootReader,
    path: &Path,
) -> Result<String, SessionMigrationError> {
    let file = reader
        .open_file(path)
        .map_err(|error| SessionMigrationError::observation(path, error))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| SessionMigrationError::observation(path, error))?;
        if available.is_empty() {
            break;
        }
        hasher.update(available);
        let consumed = available.len();
        reader.consume(consumed);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn read_legacy_header(value: &Value) -> HeaderDecision {
    let Some(object) = value.as_object() else {
        return HeaderDecision::Event;
    };
    let Some(raw_version) = object.get("norn_session_format") else {
        return HeaderDecision::Event;
    };
    if object.len() != 1 {
        return HeaderDecision::Malformed(
            "session header contains fields other than 'norn_session_format'".to_owned(),
        );
    }
    let Some(version) = raw_version
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
    else {
        return HeaderDecision::Malformed(
            "session header version is not an unsigned 32-bit integer".to_owned(),
        );
    };
    if version > 1 {
        return HeaderDecision::Malformed(format!("unsupported legacy format {version}"));
    }
    HeaderDecision::Header(version)
}

enum HeaderDecision {
    Header(u32),
    Event,
    Malformed(String),
}

fn flattened_assistant_turn(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("type").and_then(Value::as_str) != Some("AssistantMessage") {
        return false;
    }
    !matches!(object.get("response_items"), Some(Value::Array(items)) if !items.is_empty())
}

fn absorb_totals(event: &SessionEvent, totals: &mut TimelineTotals) -> Result<(), String> {
    totals.event_count = totals
        .event_count
        .checked_add(1)
        .ok_or_else(|| "event count exceeds the strict index representation".to_owned())?;
    if let SessionEvent::AssistantMessage { usage, .. } = event {
        totals.input_tokens = totals
            .input_tokens
            .checked_add(usage.input_tokens)
            .ok_or_else(|| {
                "input-token total exceeds the strict index representation".to_owned()
            })?;
        totals.output_tokens = totals
            .output_tokens
            .checked_add(usage.output_tokens)
            .ok_or_else(|| {
                "output-token total exceeds the strict index representation".to_owned()
            })?;
        totals.cache_read_tokens = totals
            .cache_read_tokens
            .checked_add(usage.cache_read_tokens)
            .ok_or_else(|| {
                "cache-read-token total exceeds the strict index representation".to_owned()
            })?;
    }
    Ok(())
}

fn invalid_timeline_reason(line: u64, diagnostic: String) -> LegacyClassificationReason {
    LegacyClassificationReason::InvalidTimeline {
        line: Some(line),
        diagnostic,
    }
}

fn stage_decode_error(path: &Path, line: u64, diagnostic: &str) -> SessionMigrationError {
    SessionMigrationError::UnrepresentableSource {
        reason: format!(
            "classified timeline {} changed shape at line {line}: {diagnostic}",
            path.display()
        ),
    }
}
