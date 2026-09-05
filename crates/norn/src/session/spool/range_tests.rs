//! Real spool range, UTF-8, private-root and registered-generation failure tests.

use std::num::NonZeroUsize;
use std::sync::Arc;

use serde_json::{Value, json};
use uuid::Uuid;

use super::{SpoolRangeError, read_file_range};
use crate::session::branch::{SessionBinding, SessionBrancher};
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::manager::{CreateSessionOptions, SessionManager};
use crate::session::persistence::SessionPersistError;
use crate::session::spool::SpoolWriter;
use crate::session::store::{
    BodyRead, DurabilityPolicy, HistoryAnchor, HistoryDirection, HistoryRead,
};
use crate::session_view::{BodyRange, BodyRepresentation, ViewError};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn demand(offset: usize, max_bytes: usize) -> Result<BodyRange, std::io::Error> {
    Ok(BodyRange {
        offset,
        max_bytes: NonZeroUsize::new(max_bytes)
            .ok_or_else(|| std::io::Error::other("fixture demand must be nonzero"))?,
    })
}

fn options() -> CreateSessionOptions {
    CreateSessionOptions {
        model: "fixture".to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    }
}

fn writer(path: &std::path::Path) -> Result<(SessionManager, SpoolWriter), SessionPersistError> {
    let manager = SessionManager::new(path);
    let opened = manager.create_with_id("range-session", options(), DurabilityPolicy::Flush)?;
    Ok((
        manager,
        SpoolWriter::for_session(path, &opened.entry, DurabilityPolicy::Flush, None),
    ))
}

fn event(writer: &SpoolWriter, value: &Value) -> Result<SessionEvent, SessionPersistError> {
    let base = EventBase::new(None);
    let spool_ref = Some(writer.write(&base.id, value)?);
    Ok(SessionEvent::ToolResult {
        base,
        tool_call_id: "call-range".to_owned(),
        tool_name: "fixture".to_owned(),
        output: json!({"bounded": true}),
        spool_ref,
        duration_ms: 1,
    })
}

fn path(writer: &SpoolWriter, event: &SessionEvent) -> std::path::PathBuf {
    writer.spool_dir().join(format!("{}.bin", event.base().id))
}

#[test]
fn requested_utf8_ranges_reconstruct_original_json_without_whole_body_decode() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (manager, owner) = writer(temp.path())?;
    let value = json!({"text": "Aé🙂Z", "extra": [1, 2]});
    let event = event(&owner, &value)?;
    let expected = serde_json::to_string(&value)?;
    let mut collected = String::new();
    let mut offset = 0;
    while offset < expected.len() {
        let chunk = owner.read_event_range(&event, demand(offset, 5)?)?;
        assert_eq!(chunk.total_bytes, expected.len());
        assert!(chunk.text.len() <= 5);
        assert_eq!(&expected[offset..chunk.end], chunk.text);
        assert!(chunk.end > offset);
        collected.push_str(&chunk.text);
        offset = chunk.end;
    }
    assert_eq!(collected, expected);
    let eof = owner.read_event_range(&event, demand(expected.len(), 5)?)?;
    assert!(eof.text.is_empty());
    assert_eq!(eof.end, expected.len());
    drop(manager);
    Ok(())
}

#[test]
fn byte_demands_refuse_split_starts_too_small_characters_and_invalid_offsets() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (manager, owner) = writer(temp.path())?;
    let event = event(&owner, &json!("é🙂"))?;
    assert!(matches!(
        owner.read_event_range(&event, demand(2, 4)?),
        Err(SpoolRangeError::Range {
            source: ViewError::InvalidRange { offset: 2 },
            ..
        })
    ));
    assert!(matches!(
        owner.read_event_range(&event, demand(1, 1)?),
        Err(SpoolRangeError::Range {
            source: ViewError::RangeTooSmall { .. },
            ..
        })
    ));
    assert!(matches!(
        owner.read_event_range(&event, demand(3, 3)?),
        Err(SpoolRangeError::Range {
            source: ViewError::RangeTooSmall { .. },
            ..
        })
    ));
    assert!(matches!(
        owner.read_event_range(&event, demand(100, 4)?),
        Err(SpoolRangeError::Range {
            source: ViewError::InvalidRange { offset: 100 },
            ..
        })
    ));
    drop(manager);
    Ok(())
}

#[test]
fn malformed_utf8_and_short_reads_are_failures_while_raw_json_stays_raw() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (manager, owner) = writer(temp.path())?;
    let event = event(&owner, &json!({"valid": true}))?;
    let file_path = path(&owner, &event);
    std::fs::write(&file_path, b"a\xffb")?;
    assert!(matches!(
        owner.read_event_range(&event, demand(0, 10)?),
        Err(SpoolRangeError::MalformedUtf8 { offset: 1, .. })
    ));
    std::fs::write(&file_path, b"a\xf0\x9f")?;
    assert!(matches!(
        owner.read_event_range(&event, demand(0, 10)?),
        Err(SpoolRangeError::MalformedUtf8 { offset: 1, .. })
    ));
    std::fs::write(&file_path, b"{not complete JSON")?;
    assert_eq!(owner.read_event_range(&event, demand(0, 4)?)?.text, "{not");
    let observed_length = std::fs::metadata(&file_path)?.len();
    let mut file = std::fs::File::open(&file_path)?;
    std::fs::write(&file_path, b"x")?;
    assert!(
        matches!(read_file_range(&mut file, &event.base().id, demand(0, 4)?, observed_length), Err(SpoolRangeError::Io { source, .. }) if source.kind() == std::io::ErrorKind::UnexpectedEof)
    );
    drop(manager);
    Ok(())
}

#[test]
fn only_the_exact_event_and_owning_root_reference_can_open() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (manager, owner) = writer(temp.path())?;
    let original = event(&owner, &json!({"body": "exact-owner"}))?;
    for reference in [
        format!("other-root/spool/{}.bin", original.base().id),
        format!("range-session/spool/{}.bin", EventId::new()),
        "../spool/escape.bin".to_owned(),
        "range-session/spool/../../escape.bin".to_owned(),
        "/outside/spool/file.bin".to_owned(),
    ] {
        let mut forged = original.clone();
        let SessionEvent::ToolResult { spool_ref, .. } = &mut forged else {
            return Err(std::io::Error::other("fixture is not a tool result").into());
        };
        *spool_ref = Some(reference);
        assert!(matches!(
            owner.read_event_range(&forged, demand(0, 10)?),
            Err(SpoolRangeError::Persistence {
                source: SessionPersistError::InvalidSpoolRef { .. },
                ..
            })
        ));
    }
    let wrong_event = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "not a spool".to_owned(),
    };
    assert!(matches!(
        owner.read_event_range(&wrong_event, demand(0, 10)?),
        Err(SpoolRangeError::Unavailable { .. })
    ));
    drop(manager);
    Ok(())
}

#[test]
fn missing_nonregular_and_recreated_generation_are_not_empty_success() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (manager, owner) = writer(temp.path())?;
    let event = event(&owner, &json!({"old": true}))?;
    let file_path = path(&owner, &event);
    std::fs::remove_file(&file_path)?;
    assert!(matches!(
        owner.read_event_range(&event, demand(0, 10)?),
        Err(SpoolRangeError::Persistence {
            source: SessionPersistError::Io(_),
            ..
        })
    ));
    std::fs::create_dir(&file_path)?;
    assert!(matches!(
        owner.read_event_range(&event, demand(0, 10)?),
        Err(SpoolRangeError::Persistence {
            source: SessionPersistError::Io(_),
            ..
        })
    ));
    std::fs::remove_dir(&file_path)?;
    manager.delete("range-session")?;
    let replacement =
        manager.create_with_id("range-session", options(), DurabilityPolicy::Flush)?;
    let new_owner = SpoolWriter::for_session(
        temp.path(),
        &replacement.entry,
        DurabilityPolicy::Flush,
        None,
    );
    new_owner.write(&event.base().id, &json!({"replacement": true}))?;
    assert!(matches!(
        owner.read_event_range(&event, demand(0, 10)?),
        Err(SpoolRangeError::Persistence {
            source: SessionPersistError::GenerationChanged { .. },
            ..
        })
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn exact_filename_symlink_cannot_escape_the_private_root() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (manager, owner) = writer(temp.path())?;
    let event = event(&owner, &json!({"own": true}))?;
    let file_path = path(&owner, &event);
    std::fs::remove_file(&file_path)?;
    let outside = tempfile::NamedTempFile::new()?;
    std::fs::write(outside.path(), b"outside-private-marker")?;
    std::os::unix::fs::symlink(outside.path(), &file_path)?;
    assert!(matches!(
        owner.read_event_range(&event, demand(0, 30)?),
        Err(SpoolRangeError::Persistence {
            source: SessionPersistError::Io(_),
            ..
        })
    ));
    assert_eq!(std::fs::read(outside.path())?, b"outside-private-marker");
    drop(manager);
    Ok(())
}

#[test]
fn store_body_read_uses_its_attached_registered_writer_and_repeats_identity() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let opened = manager.create_with_id("managed-body", options(), DurabilityPolicy::Flush)?;
    let owner = opened
        .store
        .spool()
        .ok_or_else(|| std::io::Error::other("managed store has no spool"))?;
    let event = event(owner, &json!({"raw": "é🙂"}))?;
    opened.store.append(event)?;
    let brancher = Arc::new(SessionBrancher::new(
        manager,
        opened.entry.id.clone(),
        DurabilityPolicy::Flush,
    ));
    let binding = SessionBinding::persistent_root(brancher, &opened.entry, &[]);
    let source = opened
        .store
        .bind_view_source(&binding, Uuid::new_v4(), None)?;
    let history = opened.store.history_page(&HistoryRead {
        source,
        anchor: HistoryAnchor::End,
        direction: HistoryDirection::Before,
        max_events: NonZeroUsize::MIN,
    })?;
    let reference = history
        .records
        .first()
        .and_then(|record| record.items().first())
        .and_then(|item| {
            item.bodies.iter().find(|body| {
                matches!(
                    body.origin(),
                    crate::session_view::BodyOrigin::Committed {
                        field: crate::session_view::DisplayField::ToolOutputSpool,
                        ..
                    }
                )
            })
        })
        .ok_or_else(|| std::io::Error::other("projected spool capability absent"))?
        .clone();
    let chunk = opened.store.read_body(&BodyRead {
        reference: reference.clone(),
        range: demand(0, 4)?,
    })?;
    assert_eq!(chunk.reference, reference);
    assert_eq!(chunk.reference.representation(), BodyRepresentation::Json);
    assert_eq!(chunk.range, 0..4);
    assert_eq!(chunk.text, "{\"ra");
    assert_eq!(chunk.next_offset, Some(4));
    assert_eq!(opened.store.len(), 1);
    Ok(())
}

#[test]
fn replacing_a_writer_cannot_read_new_generation_bytes_under_an_old_capability() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let mut opened =
        manager.create_with_id("replace-writer", options(), DurabilityPolicy::Flush)?;
    let owner = opened
        .store
        .spool()
        .ok_or_else(|| std::io::Error::other("managed store has no spool"))?;
    let stored_event = event(owner, &json!({"original": true}))?;
    opened.store.append(stored_event.clone())?;
    let brancher = Arc::new(SessionBrancher::new(
        manager.clone(),
        opened.entry.id.clone(),
        DurabilityPolicy::Flush,
    ));
    let binding = SessionBinding::persistent_root(brancher, &opened.entry, &[]);
    let source = opened
        .store
        .bind_view_source(&binding, Uuid::new_v4(), None)?;
    let request = HistoryRead {
        source,
        anchor: HistoryAnchor::End,
        direction: HistoryDirection::Before,
        max_events: NonZeroUsize::MIN,
    };
    let history = opened.store.history_page(&request)?;
    let reference = history
        .records
        .first()
        .and_then(|record| record.items().first())
        .and_then(|item| item.bodies.first())
        .ok_or_else(|| std::io::Error::other("spool capability missing"))?
        .clone();
    manager.delete("replace-writer")?;
    let replacement =
        manager.create_with_id("replace-writer", options(), DurabilityPolicy::Flush)?;
    let replacement_writer = SpoolWriter::for_session(
        temp.path(),
        &replacement.entry,
        DurabilityPolicy::Flush,
        None,
    );
    replacement_writer.write(&stored_event.base().id, &json!({"replacement": true}))?;
    opened.store.attach_spool(replacement_writer);
    assert!(matches!(
        opened.store.read_body(&BodyRead {
            reference,
            range: demand(0, 100)?
        }),
        Err(crate::session::store::HistoryReadError::BindingGenerationMismatch { .. })
    ));
    assert!(matches!(
        opened.store.history_page(&request),
        Err(crate::session::store::HistoryReadError::BindingGenerationMismatch { .. })
    ));
    assert_eq!(opened.store.len(), 1);
    Ok(())
}
