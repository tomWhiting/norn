use std::fs;
use std::path::Path;

use chrono::Utc;
use serde::Serialize;
use tempfile::TempDir;

use crate::session::events::{EventBase, SessionEvent};
use crate::session::persistence::SessionStatus;

use super::{
    ResumeFidelity, SessionIndexEntry, SessionRecordOrigin, StrictFormatHeader, StrictStoreError,
    validate_staged_store,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn entry(id: &str, event_count: u64) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id: id.to_owned(),
        generation: uuid::Uuid::new_v4(),
        name: None,
        model: "gpt-test".to_owned(),
        working_dir: "/workspace".to_owned(),
        created_at: now,
        updated_at: now,
        event_count,
        status: SessionStatus::Active,
        format_version: 2,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        rel_path: None,
        parent_id: None,
        fidelity: ResumeFidelity::Canonical,
        origin: SessionRecordOrigin::Native,
        provider_state_identity: None,
    }
}

fn event(content: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: content.to_owned(),
    }
}

fn write_file<T: Serialize>(path: &Path, rows: &[T]) -> TestResult {
    let mut bytes = serde_json::to_vec(&StrictFormatHeader::current())?;
    bytes.push(b'\n');
    for row in rows {
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    fs::write(path, bytes)?;
    Ok(())
}

#[test]
fn validates_manifest_and_timeline_without_runtime_cutover() -> TestResult {
    let directory = TempDir::new()?;
    let row = entry("session-a", 1);
    write_file(
        &directory.path().join("index.jsonl"),
        std::slice::from_ref(&row),
    )?;
    write_file(&directory.path().join("session-a.jsonl"), &[event("hello")])?;

    let validated = validate_staged_store(directory.path())?;
    assert_eq!(validated.index.entries.len(), 1);
    assert_eq!(validated.sessions.len(), 1);
    assert_eq!(
        validated
            .sessions
            .first()
            .map(|session| session.timeline.events.len()),
        Some(1)
    );
    Ok(())
}

#[test]
fn rejects_index_event_count_drift() -> TestResult {
    let directory = TempDir::new()?;
    let row = entry("session-a", 2);
    write_file(
        &directory.path().join("index.jsonl"),
        std::slice::from_ref(&row),
    )?;
    write_file(
        &directory.path().join("session-a.jsonl"),
        &[event("only-one")],
    )?;

    let result = validate_staged_store(directory.path());
    assert!(matches!(
        result,
        Err(StrictStoreError::EventCountMismatch {
            indexed: 2,
            actual: 1,
            ..
        })
    ));
    Ok(())
}

#[test]
fn rejects_missing_manifest_timeline() -> TestResult {
    let directory = TempDir::new()?;
    write_file(
        &directory.path().join("index.jsonl"),
        &[entry("missing", 0)],
    )?;
    let result = validate_staged_store(directory.path());
    assert!(matches!(result, Err(StrictStoreError::Io { .. })));
    Ok(())
}

#[test]
fn rejects_duplicate_resolved_timeline_paths() -> TestResult {
    let directory = TempDir::new()?;
    let root = entry("root", 0);
    let mut first = entry("child-a", 0);
    first.rel_path = Some("root/children/shared.jsonl".to_owned());
    first.parent_id = Some("root".to_owned());
    let mut second = entry("child-b", 0);
    second.rel_path = first.rel_path.clone();
    second.parent_id = Some("root".to_owned());
    fs::create_dir_all(directory.path().join("root/children"))?;
    write_file(
        &directory.path().join("root/children/shared.jsonl"),
        &[] as &[SessionEvent],
    )?;
    write_file(&directory.path().join("root.jsonl"), &[] as &[SessionEvent])?;
    write_file(
        &directory.path().join("index.jsonl"),
        &[root, first, second],
    )?;

    let result = validate_staged_store(directory.path());
    assert!(matches!(
        result,
        Err(StrictStoreError::DuplicateSessionPath { .. })
    ));
    Ok(())
}

#[test]
fn rejects_missing_parent_and_wrong_root_path() -> TestResult {
    let missing_parent_dir = TempDir::new()?;
    let mut missing_parent = entry("child", 0);
    missing_parent.parent_id = Some("missing".to_owned());
    missing_parent.rel_path = Some("missing/children/child.jsonl".to_owned());
    write_file(
        &missing_parent_dir.path().join("index.jsonl"),
        &[missing_parent],
    )?;
    assert!(matches!(
        validate_staged_store(missing_parent_dir.path()),
        Err(StrictStoreError::InvalidIndexEntry { line: 2, .. })
    ));

    let wrong_root_dir = TempDir::new()?;
    let root = entry("root", 0);
    let mut child = entry("child", 0);
    child.parent_id = Some("root".to_owned());
    child.rel_path = Some("other/children/child.jsonl".to_owned());
    write_file(&wrong_root_dir.path().join("index.jsonl"), &[root, child])?;
    assert!(matches!(
        validate_staged_store(wrong_root_dir.path()),
        Err(StrictStoreError::InvalidIndexEntry { line: 3, .. })
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn observational_validation_does_not_heal_permissions() -> TestResult {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let directory = TempDir::new()?;
    let row = entry("session-a", 0);
    let index_path = directory.path().join("index.jsonl");
    let timeline_path = directory.path().join("session-a.jsonl");
    write_file(&index_path, &[row])?;
    write_file(&timeline_path, &[] as &[SessionEvent])?;
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))?;
    fs::set_permissions(&index_path, fs::Permissions::from_mode(0o644))?;
    fs::set_permissions(&timeline_path, fs::Permissions::from_mode(0o644))?;

    let root_mode = fs::metadata(directory.path())?.mode() & 0o777;
    let index_mode = fs::metadata(&index_path)?.mode() & 0o777;
    let timeline_mode = fs::metadata(&timeline_path)?.mode() & 0o777;
    let validated = validate_staged_store(directory.path())?;
    assert!(validated.sessions.len() == 1);
    assert_eq!(fs::metadata(directory.path())?.mode() & 0o777, root_mode);
    assert_eq!(fs::metadata(&index_path)?.mode() & 0o777, index_mode);
    assert_eq!(fs::metadata(&timeline_path)?.mode() & 0o777, timeline_mode);
    Ok(())
}
