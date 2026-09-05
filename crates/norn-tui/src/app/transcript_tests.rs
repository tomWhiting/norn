//! Semantic history/body adapter tests against real in-memory store APIs.

use std::num::NonZeroUsize;

use norn::session::events::{EventBase, SessionEvent};
use norn::session::{EventStore, SessionBinding};
use norn::session_view::ViewItemKind;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn store_view() -> Result<(EventStore, Transcript), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), None)?;
    Ok((store, Transcript::new(source)))
}

#[test]
fn bounded_tail_and_older_pages_preserve_compact_identity() -> TestResult {
    let (store, mut view) = store_view()?;
    for number in 0..27 {
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: format!("message {number}"),
        })?;
    }
    assert!(view.accept_history(&store.history_page(&view.initial_history()?)?)?);
    assert_eq!(view.projection.items().len(), 20);
    assert!(view.has_older);
    assert_eq!(view.observed_events, 27);
    assert!(view.accept_history(&store.history_page(&view.older_history()?)?)?);
    assert_eq!(view.projection.items().len(), 27);
    assert!(!view.has_older);
    let count = view.projection.items().len();
    assert!(view.accept_history(&store.history_page(&view.newer_history()?)?)?);
    assert_eq!(view.projection.items().len(), count);
    Ok(())
}

#[test]
fn rotated_view_rejects_old_pages_and_body_completions() -> TestResult {
    let (store, mut old) = store_view()?;
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "private original".to_owned(),
    })?;
    let page = store.history_page(&old.initial_history()?)?;
    old.accept_history(&page)?;
    let item = old.projection.items().next().ok_or("missing item")?;
    let reference = item.bodies.first().ok_or("missing body")?.clone();
    let item_id = item.id.clone();
    let demand = old
        .demand_body(&item_id, &reference, false)?
        .ok_or("missing demand")?;
    let loaded = LoadedBody::from(store.read_body(&demand.read)?);
    let (other_store, mut fresh) = store_view()?;
    assert!(!fresh.accept_history(&page)?);
    assert!(!fresh.accept_body(&demand, loaded)?);
    assert_eq!(fresh.projection.items().len(), 0);
    assert_eq!(
        other_store
            .history_page(&fresh.initial_history()?)?
            .total_events,
        0
    );
    Ok(())
}

#[test]
fn explicit_body_ranges_keep_original_unicode_and_deduplicate_pending_work() -> TestResult {
    let (store, mut view) = store_view()?;
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "A界e\u{301}Z".to_owned(),
    })?;
    view.accept_history(&store.history_page(&view.initial_history()?)?)?;
    view.config
        .set_body_demand(NonZeroUsize::new(4).ok_or("invalid fixture demand")?);
    let item = view.projection.items().next().ok_or("missing item")?;
    let reference = item.bodies.first().ok_or("missing body")?.clone();
    let id = item.id.clone();
    let demand = view
        .demand_body(&id, &reference, false)?
        .ok_or("missing initial demand")?;
    assert!(view.demand_body(&id, &reference, false)?.is_none());
    assert!(view.accept_body(&demand, LoadedBody::from(store.read_body(&demand.read)?))?);
    assert_eq!(
        view.body(&reference).ok_or("missing prefix")?.original,
        "A界"
    );
    let next = view
        .demand_body(&id, &reference, true)?
        .ok_or("missing continuation")?;
    assert!(view.accept_body(&next, LoadedBody::from(store.read_body(&next.read)?))?);
    assert_eq!(
        view.body(&reference)
            .ok_or("missing complete body")?
            .original,
        "A界e\u{301}Z"
    );
    assert!(
        view.body(&reference)
            .ok_or("missing body")?
            .next_offset
            .is_none()
    );
    let count = view.projection.items().len();
    view.retain_bodies(&HashSet::new());
    assert!(view.body(&reference).is_none());
    assert_eq!(view.projection.items().len(), count);
    Ok(())
}

#[test]
fn body_completion_refuses_wrong_or_noncontiguous_byte_ranges() -> TestResult {
    let (store, mut view) = store_view()?;
    let id = view.notice(ViewItemKind::Notice, "fixture", Some("exact body"))?;
    let item = view.projection.item(&id).ok_or("missing notice")?;
    let reference = item.bodies.first().ok_or("missing body")?.clone();
    let demand = view
        .demand_body(&id, &reference, false)?
        .ok_or("missing demand")?;
    let mut page = view.read_local_body(&demand)?;
    page.range.start = 1;
    assert!(matches!(
        view.accept_body(&demand, page),
        Err(TuiError::InvalidBodyPage { .. })
    ));
    assert_eq!(
        store.history_page(&view.initial_history()?)?.total_events,
        0
    );
    Ok(())
}
