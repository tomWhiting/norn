//! Store-owner binding, demanded pages and capability validation regressions.

use std::num::NonZeroUsize;
use std::sync::Arc;

use uuid::Uuid;

use super::{
    BodyRead, EventStore, HistoryAnchor, HistoryDirection, HistoryRead, HistoryReadError,
    PersistenceSink,
};
use crate::session::branch::{SessionBinding, SessionBrancher};
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::manager::{CreateSessionOptions, SessionManager};
use crate::session::persistence::SessionPersistError;
use crate::session::store::DurabilityPolicy;
use crate::session_view::{
    BodyOrigin, BodyRange, BodyRef, BodyRepresentation, DisplayField, HistoryCursor,
    HistoryPosition, SessionIdentity, ViewError, ViewSource,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Sink(Vec<EventId>);

impl PersistenceSink for Sink {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        self.0.push(event.base().id.clone());
        Ok(())
    }
}

fn demand(value: usize) -> Result<NonZeroUsize, std::io::Error> {
    NonZeroUsize::new(value).ok_or_else(|| std::io::Error::other("fixture demand must be nonzero"))
}

fn user(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: text.to_owned(),
    }
}

fn source(store: &EventStore) -> Result<ViewSource, HistoryReadError> {
    store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), None)
}

fn read(
    source: &ViewSource,
    anchor: HistoryAnchor,
    direction: HistoryDirection,
    count: usize,
) -> Result<HistoryRead, std::io::Error> {
    Ok(HistoryRead {
        source: source.clone(),
        anchor,
        direction,
        max_events: demand(count)?,
    })
}

fn required<T>(value: Option<T>) -> Result<T, std::io::Error> {
    value.ok_or_else(|| std::io::Error::other("expected fixture value was absent"))
}

#[test]
fn every_constructor_mints_a_distinct_local_instance() -> TestResult {
    let stores = [
        EventStore::new(),
        EventStore::with_sink(Box::new(Sink(Vec::new()))),
        EventStore::with_sink_and_events(
            Box::new(Sink(Vec::new())),
            vec![user("already accepted")],
        ),
    ];
    let binding = SessionBinding::ephemeral_root();
    let agent = Uuid::new_v4();
    let mut generations = std::collections::HashSet::new();
    for store in stores {
        let owner = store.bind_view_source(&binding, agent, None)?;
        assert!(generations.insert(owner.store_generation));
        assert_eq!(store.bind_view_source(&binding, agent, None)?, owner);
        assert!(matches!(
            store.bind_view_source(&binding, Uuid::new_v4(), None),
            Err(HistoryReadError::View(ViewError::SourceMismatch { .. }))
        ));
        assert!(matches!(
            store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None),
            Err(HistoryReadError::BindingGenerationMismatch { .. })
        ));
    }
    Ok(())
}

#[test]
fn indexed_record_matches_page_identity_and_does_not_inspect_unselected_events() -> TestResult {
    let store = EventStore::new();
    let owner = source(&store)?;
    let first = store.append(user("same input"))?;
    let second = store.append(user("same input"))?;
    let page = store.history_page(&read(
        &owner,
        HistoryAnchor::Start,
        HistoryDirection::After,
        2,
    )?)?;
    let exact = store.history_record(&owner, &second)?;
    let paged = required(page.records.get(1))?;
    assert_eq!(exact.cursor(), paged.cursor());
    assert_eq!(exact.items().len(), 1);
    assert_eq!(exact.items()[0].id, paged.items()[0].id);
    assert_eq!(exact.items()[0].bodies, paged.items()[0].bodies);
    assert_ne!(
        exact.cursor(),
        store.history_record(&owner, &first)?.cursor()
    );

    // A corrupt unrelated entry proves this lookup does not walk or validate
    // all retained events. The exact requested index and ordinal still agree.
    store.inner.write().index.insert(first, usize::MAX);
    assert_eq!(
        store.history_record(&owner, &second)?.cursor(),
        exact.cursor()
    );
    Ok(())
}

#[test]
fn indexed_record_refuses_unbound_foreign_missing_and_inconsistent_identity() -> TestResult {
    let store = EventStore::new();
    let owner = source(&store)?;
    let id = store.append(user("selected"))?;
    let other_id = store.append(user("other"))?;
    let unbound = EventStore::new();
    assert!(matches!(
        unbound.history_record(&owner, &id),
        Err(HistoryReadError::Unbound { .. })
    ));
    let mut foreign = owner.clone();
    foreign.store_generation = Uuid::new_v4();
    assert!(matches!(
        store.history_record(&foreign, &id),
        Err(HistoryReadError::View(ViewError::SourceMismatch { .. }))
    ));
    let absent = EventId::new();
    assert!(matches!(
        store.history_record(&owner, &absent),
        Err(HistoryReadError::EventNotIndexed { event_id, .. }) if event_id == absent
    ));
    store.inner.write().index.insert(id.clone(), 1);
    assert!(matches!(
        store.history_record(&owner, &id),
        Err(HistoryReadError::View(ViewError::CursorMismatch { .. }))
    ));
    store.inner.write().index.insert(other_id.clone(), 5);
    assert!(matches!(
        store.history_record(&owner, &other_id),
        Err(HistoryReadError::EventUnavailable { ordinal: 5, .. })
    ));
    Ok(())
}

#[test]
fn indexed_record_revalidates_managed_writer_generation_after_binding() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let options = || CreateSessionOptions {
        model: "fixture".to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    };
    let first = manager.create_with_id("record-owner", options(), DurabilityPolicy::Flush)?;
    let brancher = Arc::new(SessionBrancher::new(
        manager.clone(),
        "record-owner".to_owned(),
        DurabilityPolicy::Flush,
    ));
    let binding = SessionBinding::persistent_root(brancher, &first.entry, &[]);
    let owner = first
        .store
        .bind_view_source(&binding, Uuid::new_v4(), None)?;
    let id = first.store.append(user("before replacement"))?;
    first.store.history_record(&owner, &id)?;
    manager.delete("record-owner")?;
    let replacement = manager.create_with_id("record-owner", options(), DurabilityPolicy::Flush)?;
    assert_ne!(first.entry.generation, replacement.entry.generation);
    assert!(matches!(
        first.store.history_record(&owner, &id),
        Err(HistoryReadError::CurrentOwner { .. })
    ));
    Ok(())
}

#[test]
fn explicit_pages_preserve_order_without_repeating_exclusive_anchors() -> TestResult {
    let store = EventStore::new();
    let owner = source(&store)?;
    let mut ids = Vec::new();
    for index in 0..5 {
        ids.push(store.append(user(&format!("body-{index}")))?);
    }
    let tail = store.history_page(&read(
        &owner,
        HistoryAnchor::End,
        HistoryDirection::Before,
        2,
    )?)?;
    assert_eq!(tail.records.len(), 2);
    assert!(tail.has_before);
    assert!(!tail.has_after);
    assert_eq!(tail.total_events, 5);
    assert!(
        matches!(tail.records[0].cursor().position(), HistoryPosition::Event { ordinal: 3, event_id } if event_id == &ids[3])
    );
    let older = store.history_page(&read(
        &owner,
        HistoryAnchor::At(required(tail.next)?),
        HistoryDirection::Before,
        2,
    )?)?;
    assert!(
        matches!(older.records[0].cursor().position(), HistoryPosition::Event { ordinal: 1, event_id } if event_id == &ids[1])
    );
    let first = store.history_page(&read(
        &owner,
        HistoryAnchor::At(required(older.next)?),
        HistoryDirection::Before,
        2,
    )?)?;
    assert_eq!(first.records.len(), 1);
    assert!(!first.has_before);
    assert!(first.has_after);
    let all_after = store.history_page(&read(
        &owner,
        HistoryAnchor::At(required(first.next)?),
        HistoryDirection::After,
        10,
    )?)?;
    assert_eq!(all_after.records.len(), 4);
    assert!(matches!(
        all_after.records[0].cursor().position(),
        HistoryPosition::Event { ordinal: 1, .. }
    ));
    let last = required(all_after.next)?;
    store.append(user("appended later"))?;
    let suffix = store.history_page(&read(
        &owner,
        HistoryAnchor::At(last),
        HistoryDirection::After,
        2,
    )?)?;
    assert_eq!(suffix.records.len(), 1);
    assert_eq!(suffix.total_events, 6);
    Ok(())
}

#[test]
fn empty_start_and_wrong_source_position_and_event_are_explicit() -> TestResult {
    let store = EventStore::new();
    let owner = source(&store)?;
    let start = store.history_start(&owner)?;
    assert_eq!(start.position(), &HistoryPosition::Empty);
    let empty = store.history_page(&read(
        &owner,
        HistoryAnchor::At(start.clone()),
        HistoryDirection::After,
        2,
    )?)?;
    assert!(empty.records.is_empty());
    assert!(empty.next.is_none());
    assert!(!empty.has_after && !empty.has_before);
    let id = store.append(user("first"))?;
    assert_eq!(
        store
            .history_page(&read(
                &owner,
                HistoryAnchor::At(start),
                HistoryDirection::After,
                1
            )?)?
            .records
            .len(),
        1
    );
    let mut wrong_source = owner.clone();
    wrong_source.store_generation = Uuid::new_v4();
    assert!(matches!(
        store.history_page(&read(
            &wrong_source,
            HistoryAnchor::Start,
            HistoryDirection::After,
            1
        )?),
        Err(HistoryReadError::View(ViewError::SourceMismatch { .. }))
    ));
    let foreign = HistoryCursor::empty(wrong_source);
    assert!(
        store
            .history_page(&read(
                &owner,
                HistoryAnchor::At(foreign),
                HistoryDirection::After,
                1
            )?)
            .is_err()
    );
    let wrong_ordinal = HistoryCursor::event(owner.clone(), 2, id);
    assert!(matches!(
        store.history_page(&read(
            &owner,
            HistoryAnchor::At(wrong_ordinal),
            HistoryDirection::After,
            1
        )?),
        Err(HistoryReadError::EventUnavailable { ordinal: 2, .. })
    ));
    let wrong_id = HistoryCursor::event(owner.clone(), 0, EventId::new());
    assert!(matches!(
        store.history_page(&read(
            &owner,
            HistoryAnchor::At(wrong_id),
            HistoryDirection::After,
            1
        )?),
        Err(HistoryReadError::View(ViewError::CursorMismatch { .. }))
    ));
    let reopened =
        EventStore::with_sink_and_events(Box::new(Sink(Vec::new())), vec![user("other instance")]);
    source(&reopened)?;
    assert!(reopened.history_start(&owner).is_err());
    Ok(())
}

#[test]
fn compact_pages_do_not_retain_unknown_custom_payloads() -> TestResult {
    let store = EventStore::new();
    let owner = source(&store)?;
    store.append(SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: "unknown-provider-state".to_owned(),
        data: serde_json::json!({"opaque": "private-provider-state-marker"}),
    })?;
    store.append(user("lazy-user-body-marker"))?;
    let page = store.history_page(&read(
        &owner,
        HistoryAnchor::Start,
        HistoryDirection::After,
        2,
    )?)?;
    let retained = format!("{page:?}");
    assert!(!retained.contains("private-provider-state-marker"));
    assert!(!retained.contains("lazy-user-body-marker"));
    assert_eq!(page.records.len(), 2);
    Ok(())
}

#[test]
fn inline_ranges_bind_event_field_representation_and_generation() -> TestResult {
    let store = EventStore::new();
    let owner = source(&store)?;
    let event = user("Aé🙂Z");
    store.append(event.clone())?;
    let page = store.history_page(&read(
        &owner,
        HistoryAnchor::Start,
        HistoryDirection::After,
        1,
    )?)?;
    let item = required(page.records[0].items().first())?;
    let reference = required(item.bodies.first())?.clone();
    let chunk = store.read_body(&BodyRead {
        reference: reference.clone(),
        range: BodyRange {
            offset: 1,
            max_bytes: demand(5)?,
        },
    })?;
    assert_eq!(chunk.text, "é");
    assert_eq!(chunk.range, 1..3);
    assert_eq!(chunk.next_offset, Some(3));
    assert_eq!(chunk.total_bytes, 8);
    assert_eq!(chunk.reference, reference);
    assert!(
        store
            .read_body(&BodyRead {
                reference,
                range: BodyRange {
                    offset: 2,
                    max_bytes: demand(4)?
                }
            })
            .is_err()
    );
    let cursor = page.records[0].cursor().clone();
    for (field, representation) in [
        (DisplayField::ToolOutputInline, BodyRepresentation::Json),
        (DisplayField::UserContent, BodyRepresentation::Json),
    ] {
        let forged = BodyRef {
            origin: BodyOrigin::Committed {
                cursor: cursor.clone(),
                field,
                representation,
            },
        };
        assert!(matches!(
            store.read_body(&BodyRead {
                reference: forged,
                range: BodyRange {
                    offset: 0,
                    max_bytes: demand(8)?
                }
            }),
            Err(HistoryReadError::View(ViewError::FieldUnavailable { .. }))
        ));
    }
    let projected = BodyRef {
        origin: BodyOrigin::Local {
            source: owner,
            ordinal: 1,
            revision: 7,
            representation: BodyRepresentation::Text,
        },
    };
    assert!(matches!(
        store.read_body(&BodyRead {
            reference: projected,
            range: BodyRange {
                offset: 0,
                max_bytes: demand(8)?
            }
        }),
        Err(HistoryReadError::ProjectionOwned { revision: 7 })
    ));
    assert_eq!(store.len(), 1);
    assert!(
        matches!(store.get(&event.base().id), Some(SessionEvent::UserMessage { content, .. }) if content == "Aé🙂Z")
    );
    Ok(())
}

#[test]
fn managed_binding_rejects_same_session_with_another_index_generation() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let options = || CreateSessionOptions {
        model: "fixture".to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    };
    let first = manager.create_with_id("same-id", options(), DurabilityPolicy::Flush)?;
    let brancher = Arc::new(SessionBrancher::new(
        manager.clone(),
        "same-id".to_owned(),
        DurabilityPolicy::Flush,
    ));
    let first_binding = SessionBinding::persistent_root(Arc::clone(&brancher), &first.entry, &[]);
    manager.delete("same-id")?;
    let replacement = manager.create_with_id("same-id", options(), DurabilityPolicy::Flush)?;
    let replacement_binding = SessionBinding::persistent_root(brancher, &replacement.entry, &[]);
    assert_ne!(first.entry.generation, replacement.entry.generation);
    assert!(matches!(
        first
            .store
            .bind_view_source(&replacement_binding, Uuid::new_v4(), None),
        Err(HistoryReadError::BindingGenerationMismatch { .. })
    ));
    assert!(matches!(
        replacement
            .store
            .bind_view_source(&first_binding, Uuid::new_v4(), None),
        Err(HistoryReadError::BindingGenerationMismatch { .. })
    ));
    let bound = replacement
        .store
        .bind_view_source(&replacement_binding, Uuid::new_v4(), None)?;
    assert_eq!(
        bound.session,
        SessionIdentity::Persisted("same-id".to_owned())
    );
    Ok(())
}

#[test]
fn corrupted_loaded_index_and_body_source_do_not_mint_or_read_capabilities() -> TestResult {
    let event = user("source-bound-body");
    let corrupt = EventStore::with_sink_and_events(
        Box::new(Sink(Vec::new())),
        vec![event.clone(), event.clone()],
    );
    let corrupt_source = source(&corrupt)?;
    assert!(matches!(
        corrupt.history_page(&read(
            &corrupt_source,
            HistoryAnchor::Start,
            HistoryDirection::After,
            1
        )?),
        Err(HistoryReadError::View(ViewError::HistoryConflict {
            ordinal: 0,
            ..
        }))
    ));
    let store = EventStore::new();
    let owner = source(&store)?;
    store.append(event.clone())?;
    let mut wrong_source = owner;
    wrong_source.session = SessionIdentity::Persisted("another-session".to_owned());
    let reference = BodyRef::committed(
        HistoryCursor::event(wrong_source, 0, event.base().id.clone()),
        &event,
        DisplayField::UserContent,
    )?;
    assert!(matches!(
        store.read_body(&BodyRead {
            reference,
            range: BodyRange {
                offset: 0,
                max_bytes: demand(8)?
            }
        }),
        Err(HistoryReadError::View(ViewError::SourceMismatch { .. }))
    ));
    Ok(())
}
