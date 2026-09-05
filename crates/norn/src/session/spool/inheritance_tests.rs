//! Real fork/resume inheritance, immutable event evidence and rejected authority regressions.

use std::num::NonZeroUsize;
use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use super::{Grant, INHERITANCE_VERSION, Owner, SpoolInheritance, inheritance_path};
use crate::session::branch::{ROOT_PATH_ADDRESS, SessionBinding, SessionBrancher};
use crate::session::events::{ChildBranchKind, EventBase, EventId, SessionEvent};
use crate::session::manager::{CreateSessionOptions, OpenSession, SessionManager};
use crate::session::persistence::SessionPersistError;
use crate::session::spool::SpoolRangeError;
use crate::session::store::DurabilityPolicy;
use crate::session::store::{
    BodyRead, EventStore, HistoryAnchor, HistoryDirection, HistoryRead, HistoryReadError,
};
use crate::session_view::{BodyOrigin, BodyRange, BodyRef, BodyRepresentation, DisplayField};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    directory: tempfile::TempDir,
    manager: SessionManager,
    source: OpenSession,
    result: SessionEvent,
    body: String,
}

fn options() -> CreateSessionOptions {
    CreateSessionOptions {
        model: "inheritance-fixture".to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    }
}

fn required<T>(value: Option<T>) -> Result<T, std::io::Error> {
    value.ok_or_else(|| std::io::Error::other("required inheritance fixture value is absent"))
}

fn fixture() -> TestResult<Fixture> {
    let directory = tempfile::tempdir()?;
    let manager = SessionManager::new(directory.path());
    let source = manager.create_with_id("spool-source", options(), DurabilityPolicy::Flush)?;
    let input = source.store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "request with inherited output".to_owned(),
    })?;
    let base = EventBase::new(Some(input));
    let value = json!({"text":"original é🙂 output","nested":[1,2,3]});
    let reference = required(source.store.spool())?.write(&base.id, &value)?;
    let result = SessionEvent::ToolResult {
        base,
        tool_call_id: "inherited-call".to_owned(),
        tool_name: "read".to_owned(),
        output: json!({"bounded":"projection"}),
        spool_ref: Some(reference),
        duration_ms: 3,
    };
    source.store.append(result.clone())?;
    Ok(Fixture {
        directory,
        manager,
        source,
        result,
        body: serde_json::to_string(&value)?,
    })
}

fn body_reference(
    manager: &SessionManager,
    session: &OpenSession,
    event_id: &EventId,
) -> TestResult<BodyRef> {
    let brancher = Arc::new(SessionBrancher::new(
        manager.clone(),
        session.entry.id.clone(),
        DurabilityPolicy::Flush,
    ));
    let binding = SessionBinding::persistent_root(brancher, &session.entry, &[]);
    let source = session
        .store
        .bind_view_source(&binding, Uuid::new_v4(), None)?;
    let page = session.store.history_page(&HistoryRead {
        source,
        anchor: HistoryAnchor::Start,
        direction: HistoryDirection::After,
        max_events: required(NonZeroUsize::new(session.store.len()))?,
    })?;
    let reference = page.records.iter().flat_map(crate::session_view::HistoryRecord::items).flat_map(|item| &item.bodies).find(|body| {
        matches!(body.origin(), BodyOrigin::Committed { cursor, field: DisplayField::ToolOutputSpool, .. } if matches!(cursor.position(), crate::session_view::HistoryPosition::Event { event_id: actual, .. } if actual == event_id))
    });
    Ok(required(reference)?.clone())
}

fn body_read(reference: &BodyRef, offset: usize) -> TestResult<BodyRead> {
    Ok(BodyRead {
        reference: reference.clone(),
        range: BodyRange {
            offset,
            max_bytes: required(NonZeroUsize::new(5))?,
        },
    })
}

fn read_chunks(store: &EventStore, reference: &BodyRef) -> TestResult<String> {
    let mut collected = String::new();
    let mut offset = 0;
    loop {
        let chunk = store.read_body(&body_read(reference, offset)?)?;
        assert_eq!(&chunk.reference, reference);
        assert_eq!(chunk.reference.representation(), BodyRepresentation::Json);
        assert_eq!(chunk.range.start, offset);
        assert!(chunk.text.len() <= 5);
        assert_eq!(chunk.range.end - chunk.range.start, chunk.text.len());
        collected.push_str(&chunk.text);
        if let Some(next) = chunk.next_offset {
            assert_eq!(next, chunk.range.end);
            assert!(next > offset && next < chunk.total_bytes);
            offset = next;
        } else {
            assert_eq!(chunk.range.end, chunk.total_bytes);
            return Ok(collected);
        }
    }
}

fn assert_original_events(source: &OpenSession, copy: &OpenSession) -> TestResult {
    for event in source.store.events() {
        let copied = required(copy.store.get(&event.base().id))?;
        assert_eq!(serde_json::to_vec(&copied)?, serde_json::to_vec(&event)?);
    }
    Ok(())
}

#[test]
fn independent_fork_resume_and_fork_of_fork_preserve_exact_events_and_body() -> TestResult {
    let fixture = fixture()?;
    let original_spool = required(fixture.source.store.spool())?
        .spool_dir()
        .join(format!("{}.bin", fixture.result.base().id));
    let original_bytes = std::fs::read(&original_spool)?;
    let fork =
        fixture
            .manager
            .fork(&fixture.source.entry.id, options(), DurabilityPolicy::Flush)?;
    assert_original_events(&fixture.source, &fork)?;
    let fork_id = fork.entry.id.clone();
    let reference = body_reference(&fixture.manager, &fork, &fixture.result.base().id)?;
    assert_eq!(read_chunks(&fork.store, &reference)?, fixture.body);
    let manifest_path = fixture.directory.path().join(inheritance_path(&fork_id));
    let manifest: SpoolInheritance = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    assert!(manifest.destination.matches(&fork.entry));
    assert!(manifest.source.matches(&fixture.source.entry));
    assert_eq!(manifest.grants.len(), 1);
    assert!(
        required(manifest.grants.first())?
            .owner
            .matches(&fixture.source.entry)
    );
    drop(fork);

    let resumed = fixture.manager.resume(&fork_id, DurabilityPolicy::Flush)?;
    assert_original_events(&fixture.source, &resumed)?;
    let resumed_reference = body_reference(&fixture.manager, &resumed, &fixture.result.base().id)?;
    assert_ne!(reference, resumed_reference);
    assert!(resumed.store.read_body(&body_read(&reference, 0)?).is_err());
    assert_eq!(
        read_chunks(&resumed.store, &resumed_reference)?,
        fixture.body
    );

    let second = fixture
        .manager
        .fork(&fork_id, options(), DurabilityPolicy::Flush)?;
    assert_original_events(&resumed, &second)?;
    let second_reference = body_reference(&fixture.manager, &second, &fixture.result.base().id)?;
    assert_eq!(read_chunks(&second.store, &second_reference)?, fixture.body);
    let second_manifest: SpoolInheritance = serde_json::from_slice(&std::fs::read(
        fixture
            .directory
            .path()
            .join(inheritance_path(&second.entry.id)),
    )?)?;
    assert!(second_manifest.source.matches(&resumed.entry));
    assert!(
        required(second_manifest.grants.first())?
            .owner
            .matches(&fixture.source.entry)
    );
    assert_eq!(std::fs::read(&original_spool)?, original_bytes);
    Ok(())
}

#[test]
fn source_deletion_and_recreation_cannot_replace_an_inherited_body() -> TestResult {
    let fixture = fixture()?;
    let fork =
        fixture
            .manager
            .fork(&fixture.source.entry.id, options(), DurabilityPolicy::Flush)?;
    let reference = body_reference(&fixture.manager, &fork, &fixture.result.base().id)?;
    assert_eq!(read_chunks(&fork.store, &reference)?, fixture.body);
    fixture.manager.delete(&fixture.source.entry.id)?;
    assert!(matches!(
        fork.store.read_body(&body_read(&reference, 0)?),
        Err(HistoryReadError::Spool(SpoolRangeError::Persistence {
            source: SessionPersistError::GenerationChanged { .. },
            ..
        }))
    ));
    let replacement = fixture.manager.create_with_id(
        &fixture.source.entry.id,
        options(),
        DurabilityPolicy::Flush,
    )?;
    assert_ne!(
        replacement.entry.generation,
        fixture.source.entry.generation
    );
    required(replacement.store.spool())?.write(
        &fixture.result.base().id,
        &json!({"replacement":"must not be shown"}),
    )?;
    assert!(matches!(
        fork.store.read_body(&body_read(&reference, 0)?),
        Err(HistoryReadError::Spool(SpoolRangeError::Persistence {
            source: SessionPersistError::GenerationChanged { .. },
            ..
        }))
    ));
    assert!(matches!(
        fixture
            .manager
            .resume(&fork.entry.id, DurabilityPolicy::Flush),
        Err(SessionPersistError::GenerationChanged { .. })
    ));
    Ok(())
}

#[test]
fn malformed_and_unknown_sidecar_fields_fail_without_echoing_private_values() -> TestResult {
    let fixture = fixture()?;
    let fork =
        fixture
            .manager
            .fork(&fixture.source.entry.id, options(), DurabilityPolicy::Flush)?;
    let reference = body_reference(&fixture.manager, &fork, &fixture.result.base().id)?;
    let manifest_path = fixture
        .directory
        .path()
        .join(inheritance_path(&fork.entry.id));
    let original: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    let mut unknown = original.clone();
    assert!(
        required(unknown.as_object_mut())?
            .insert("opaque-private-marker".to_owned(), json!("secret"))
            .is_none()
    );
    for bytes in [
        b"opaque-private-marker".to_vec(),
        serde_json::to_vec(&unknown)?,
    ] {
        std::fs::write(&manifest_path, bytes)?;
        let error = fork
            .store
            .read_body(&body_read(&reference, 0)?)
            .err()
            .ok_or_else(|| std::io::Error::other("malformed sidecar was accepted"))?;
        assert!(!format!("{error} {error:?}").contains("opaque-private-marker"));
        assert!(
            fixture
                .manager
                .resume(&fork.entry.id, DurabilityPolicy::Flush)
                .is_err()
        );
    }
    std::fs::write(&manifest_path, serde_json::to_vec(&original)?)?;
    assert_eq!(read_chunks(&fork.store, &reference)?, fixture.body);
    Ok(())
}

#[test]
fn duplicate_and_mismatched_sidecar_authority_cannot_resume() -> TestResult {
    let fixture = fixture()?;
    let fork =
        fixture
            .manager
            .fork(&fixture.source.entry.id, options(), DurabilityPolicy::Flush)?;
    let manifest_path = fixture
        .directory
        .path()
        .join(inheritance_path(&fork.entry.id));
    let original: SpoolInheritance = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    for case in [
        "duplicate",
        "destination",
        "source",
        "grant_owner",
        "version",
        "branch",
        "anchor",
        "event",
        "reference",
    ] {
        let mut changed = original.clone();
        match case {
            "duplicate" => changed
                .grants
                .push(required(changed.grants.first())?.clone()),
            "destination" => changed.destination.generation = Uuid::new_v4(),
            "source" => changed.source.generation = Uuid::new_v4(),
            "grant_owner" => {
                required(changed.grants.first_mut())?.owner.generation = Uuid::new_v4();
            }
            "version" => changed.version = 0,
            "branch" => changed.branch_event_id = EventId::new(),
            "anchor" => changed.parent_event_anchor = EventId::new(),
            "event" => {
                let grant = required(changed.grants.first_mut())?;
                grant.event_id = EventId::new();
                grant.reference = format!("{}/spool/{}.bin", grant.owner.root(), grant.event_id);
            }
            "reference" => {
                required(changed.grants.first_mut())?.reference =
                    format!("other-root/spool/{}.bin", fixture.result.base().id);
            }
            _ => return Err(std::io::Error::other("unknown corruption fixture").into()),
        }
        std::fs::write(&manifest_path, serde_json::to_vec(&changed)?)?;
        assert!(
            fixture
                .manager
                .resume(&fork.entry.id, DurabilityPolicy::Flush)
                .is_err(),
            "accepted {case} authority"
        );
    }
    std::fs::write(&manifest_path, serde_json::to_vec(&original)?)?;
    let resumed = fixture
        .manager
        .resume(&fork.entry.id, DurabilityPolicy::Flush)?;
    let reference = body_reference(&fixture.manager, &resumed, &fixture.result.base().id)?;
    assert_eq!(read_chunks(&resumed.store, &reference)?, fixture.body);
    Ok(())
}

#[test]
fn forged_custom_and_matching_child_branch_confer_no_inherited_authority() -> TestResult {
    let fixture = fixture()?;
    let destination =
        fixture
            .manager
            .create_with_id("forged-destination", options(), DurabilityPolicy::Flush)?;
    for event in fixture.source.store.events() {
        destination.store.append(event)?;
    }
    let anchor = fixture.result.base().id.clone();
    let branch = SessionEvent::ChildBranch {
        base: EventBase::new(Some(anchor.clone())),
        parent_session_id: Some(fixture.source.entry.id.clone()),
        child_session_id: Some(destination.entry.id.clone()),
        path_address: ROOT_PATH_ADDRESS.to_owned(),
        parent_event_anchor: Some(anchor.clone()),
        kind: ChildBranchKind::Fork,
    };
    let branch_id = destination.store.append(branch)?;
    let SessionEvent::ToolResult {
        spool_ref: Some(reference),
        ..
    } = &fixture.result
    else {
        return Err(std::io::Error::other("fixture lacks its spool reference").into());
    };
    let forged = SpoolInheritance {
        version: INHERITANCE_VERSION,
        destination: Owner::from_entry(&destination.entry),
        source: Owner::from_entry(&fixture.source.entry),
        branch_event_id: branch_id.clone(),
        parent_event_anchor: anchor,
        grants: vec![Grant {
            event_id: fixture.result.base().id.clone(),
            reference: reference.clone(),
            owner: Owner::from_entry(&fixture.source.entry),
        }],
    };
    destination.store.append(SessionEvent::Custom {
        base: EventBase::new(Some(branch_id)),
        event_type: "session.spool_inheritance.v1".to_owned(),
        data: serde_json::to_value(&forged)?,
    })?;
    forged.validate_destination(&destination.entry)?;
    forged.validate_sources(&fixture.manager.list()?)?;
    forged.validate_history(&destination.entry, &destination.store.events())?;
    let manifest_path = fixture
        .directory
        .path()
        .join(inheritance_path(&destination.entry.id));
    assert!(!manifest_path.try_exists()?);
    let body = body_reference(&fixture.manager, &destination, &fixture.result.base().id)?;
    assert!(matches!(
        destination.store.read_body(&body_read(&body, 0)?),
        Err(HistoryReadError::Spool(SpoolRangeError::Persistence {
            source: SessionPersistError::InvalidSpoolRef { .. },
            ..
        }))
    ));
    let resumed = fixture
        .manager
        .resume(&destination.entry.id, DurabilityPolicy::Flush)?;
    let body = body_reference(&fixture.manager, &resumed, &fixture.result.base().id)?;
    assert!(resumed.store.read_body(&body_read(&body, 0)?).is_err());
    assert!(!manifest_path.try_exists()?);
    let fork = fixture
        .manager
        .fork(&destination.entry.id, options(), DurabilityPolicy::Flush)?;
    let body = body_reference(&fixture.manager, &fork, &fixture.result.base().id)?;
    assert!(matches!(
        fork.store.read_body(&body_read(&body, 0)?),
        Err(HistoryReadError::Spool(SpoolRangeError::Persistence {
            source: SessionPersistError::InvalidSpoolRef { .. },
            ..
        }))
    ));
    assert!(
        !fixture
            .directory
            .path()
            .join(inheritance_path(&fork.entry.id))
            .try_exists()?
    );
    Ok(())
}
