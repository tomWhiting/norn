//! Real store frontier, cancellation and publication-bound Latest hit regressions.

use super::*;
use crate::app::transcript::Transcript;
use crate::render::layout::Layout;
use norn::session::events::{EventBase, SessionEvent};
use norn::session::{EventStore, SessionBinding};
use std::num::NonZeroUsize;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture() -> TestResult<(Arc<EventStore>, Transcript)> {
    let store = Arc::new(EventStore::new());
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), None)?;
    let mut view = Transcript::new(source);
    view.config
        .set_history_demand(NonZeroUsize::new(2).ok_or("fixture demand invalid")?);
    Ok((store, view))
}

fn append(store: &EventStore, count: usize) -> TestResult {
    for index in 0..count {
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: format!("record {index}"),
        })?;
    }
    Ok(())
}

#[test]
fn fixed_frontier_uses_actual_zero_based_ordinals_and_does_not_chase_appends() -> TestResult {
    let (store, mut view) = fixture()?;
    append(&store, 1)?;
    view.accept_history(&store.history_page(&view.initial_history()?)?)?;
    append(&store, 5)?;
    let mut latest = LatestHistory::default();
    latest.begin(view.projection.source());
    assert!(latest.start());
    let first_read = view.newer_history()?;
    let first = store.history_page(&first_read)?;
    assert_eq!(first.total_events, 6);
    assert_eq!(
        position_count(
            first
                .records
                .last()
                .ok_or("first page empty")?
                .cursor()
                .position()
        )?,
        3
    );
    latest.observe(&first_read, &first)?;
    view.accept_history(&first)?;
    assert!(latest.pending());
    append(&store, 7)?;
    assert!(latest.start());
    let second_read = view.newer_history()?;
    let second = store.history_page(&second_read)?;
    assert_eq!(second.total_events, 13);
    latest.observe(&second_read, &second)?;
    view.accept_history(&second)?;
    assert!(latest.pending());
    assert_eq!(
        latest
            .request
            .as_ref()
            .ok_or("captured frontier missing")?
            .frontier,
        Some(6)
    );
    assert!(latest.start());
    let last_read = view.newer_history()?;
    let last = store.history_page(&last_read)?;
    latest.observe(&last_read, &last)?;
    assert!(
        last.has_after,
        "later appends must still exist beyond completed captured coverage"
    );
    assert!(!latest.pending());
    Ok(())
}

#[test]
fn empty_after_proof_is_bound_to_actual_source_and_current_cursor() -> TestResult {
    let (store, mut view) = fixture()?;
    append(&store, 3)?;
    view.accept_history(&store.history_page(&view.initial_history()?)?)?;
    let mut latest = LatestHistory::default();
    latest.begin(view.projection.source());
    assert!(latest.start());
    let read = view.newer_history()?;
    let page = store.history_page(&read)?;
    assert!(page.records.is_empty());
    assert_eq!(page.total_events, 3);
    latest.observe(&read, &page)?;
    assert!(!latest.pending());
    let (other, foreign) = fixture()?;
    let foreign_read = foreign.initial_history()?;
    latest.begin(view.projection.source());
    assert!(latest.start());
    assert!(
        latest
            .observe(&foreign_read, &other.history_page(&foreign_read)?)
            .is_err()
    );
    assert!(!latest.pending());
    Ok(())
}

#[test]
fn empty_initial_store_completes_but_truncated_frontier_reports_refusal() -> TestResult {
    let (store, mut view) = fixture()?;
    let mut latest = LatestHistory::default();
    latest.begin(view.projection.source());
    assert!(latest.start());
    let read = view.initial_history()?;
    latest.observe(&read, &store.history_page(&read)?)?;
    assert!(!latest.pending());
    append(&store, 1)?;
    view.accept_history(&store.history_page(&view.initial_history()?)?)?;
    append(&store, 3)?;
    latest.begin(view.projection.source());
    assert!(latest.start());
    let read = view.newer_history()?;
    let mut page = store.history_page(&read)?;
    // A corrupted completion cannot claim the captured frontier merely by being empty.
    page.records.clear();
    assert!(latest.observe(&read, &page).is_err());
    assert!(!latest.pending());
    Ok(())
}

#[tokio::test]
async fn latest_waits_for_existing_older_job_and_cancelled_completion_never_reenables_it()
-> TestResult {
    let (store, mut view) = fixture()?;
    append(&store, 7)?;
    view.accept_history(&store.history_page(&view.initial_history()?)?)?;
    assert!(view.load_older(&store)?);
    view.request_latest();
    assert!(!view.load_latest(&store)?);
    assert_eq!(view.history_tasks.len(), 1);
    let result = view
        .history_tasks
        .join_next()
        .await
        .ok_or("older job missing")?;
    view.finish_history(result)?;
    assert!(view.latest_pending());
    assert!(view.load_latest(&store)?);
    view.cancel_latest();
    let result = view
        .history_tasks
        .join_next()
        .await
        .ok_or("latest job missing")?;
    view.finish_history(result)?;
    assert!(!view.latest_pending());
    assert!(!view.load_latest(&store)?);
    assert!(view.history_tasks.is_empty());
    Ok(())
}

#[test]
fn replacement_intent_is_not_completed_or_failed_by_retired_job() -> TestResult {
    let (store, view) = fixture()?;
    let read = view.initial_history()?;
    let page = store.history_page(&read)?;
    let mut latest = LatestHistory::default();
    latest.begin(view.projection.source());
    assert!(latest.start());
    latest.begin(view.projection.source());
    assert!(!latest.start());
    latest.observe(&read, &page)?;
    assert!(latest.pending());
    assert!(latest.start());
    latest.cancel();
    latest.begin(view.projection.source());
    latest.failed();
    assert!(latest.pending());
    assert!(latest.start());
    latest.failed();
    assert!(!latest.pending());
    Ok(())
}

#[test]
fn latest_hit_requires_published_frame_and_source_and_does_not_change_draft_or_focus() -> TestResult
{
    let (store, view) = fixture()?;
    let mut state = AppState::new(
        crate::terminal::caps::TerminalCaps::baseline(),
        crate::input::history::InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        view.projection.source().clone(),
        crate::render::fixed_panel::StatusBar::default(),
    );
    state.input_editor.paste_cells("draft remains")?;
    state.screen.viewport.pin();
    let focus = state.screen.focus;
    let area = Rect {
        column: 70,
        row: 23,
        width: 8,
        height: 1,
    };
    state.screen.prepared_latest = Some(area);
    assert!(
        !activate(&mut state, 71, 23),
        "preparation grants no hit authority"
    );
    let frame = Arc::new(Frame {
        layout: Layout::NoPaint,
        rows: Vec::new(),
        composer: None,
        cursor: None,
    });
    finish_publication(&mut state.screen, Arc::clone(&frame), Ok(()))?;
    assert!(!activate(&mut state, 69, 23));
    assert!(activate(&mut state, 71, 23));
    assert!(state.screen.viewport.follows_tail());
    assert!(state.transcript.latest_pending());
    assert_eq!(state.input_editor.text(), "draft remains");
    assert_eq!(state.screen.focus, focus);
    assert_eq!(
        store.history_page(&view.initial_history()?)?.total_events,
        0
    );
    state.screen.viewport.pin();
    state.screen.prepared_latest = Some(area);
    let failed = finish_publication(
        &mut state.screen,
        Arc::clone(&frame),
        Err(interaction(std::io::Error::other("fixture flush failure"))),
    );
    assert!(failed.is_err());
    assert!(state.screen.prepared_latest.is_none());
    assert!(state.screen.display_frame.is_none());
    assert!(
        !activate(&mut state, 71, 23),
        "failed publication cannot remain clickable"
    );
    state.screen.prepared_latest = Some(area);
    finish_publication(&mut state.screen, frame, Ok(()))?;
    state
        .screen
        .replace_source(&crate::app::state::test_view_source(Uuid::new_v4()));
    assert!(!activate(&mut state, 71, 23));
    Ok(())
}
