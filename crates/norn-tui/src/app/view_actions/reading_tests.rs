//! Reading-action integration against actual source capabilities and supervised filesystem jobs.

use norn::agent::registry::AgentRegistry;
use norn::session::events::{EventBase, SessionEvent};
use norn::session::{EventStore, SessionBinding};
use norn::session_view::ViewItemKind;
use parking_lot::RwLock;
use std::sync::Arc;
use uuid::Uuid;

use super::*;
use crate::input::history::InputHistory;
use crate::render::fixed_panel::StatusBar;
use crate::terminal::caps::TerminalCaps;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture() -> Result<(Arc<EventStore>, AppState), Box<dyn std::error::Error>> {
    let store = Arc::new(EventStore::new());
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), None)?;
    let state = AppState::new(
        TerminalCaps::baseline(),
        InputHistory::in_memory(),
        Arc::new(RwLock::new(AgentRegistry::new())),
        source,
        StatusBar::default(),
    );
    Ok((store, state))
}

fn body(
    state: &mut AppState,
    store: &Arc<EventStore>,
    text: &str,
) -> Result<ItemId, Box<dyn std::error::Error>> {
    let id = state
        .transcript
        .notice(ViewItemKind::Notice, "original fixture", Some(text))?;
    let reference = state
        .transcript
        .projection
        .item(&id)
        .and_then(|item| item.bodies.first())
        .ok_or("missing fixture body")?
        .clone();
    state.transcript.load_body(store, &id, &reference, false)?;
    Ok(id)
}

#[test]
fn search_hit_uses_original_graphemes_and_refuses_an_evicted_revision() -> TestResult {
    let (store, mut state) = fixture()?;
    let id = body(&mut state, &store, "**e\u{301}** 👩‍💻 e\u{301}\nlast")?;
    search(&mut state, SearchScope::LoadedTranscript, "e\u{301}")?;
    assert_eq!(state.screen.selection_item.as_ref(), Some(&id));
    assert_eq!(super::super::selected_text(&state)?, "e\u{301}");
    assert_eq!(state.screen.search.hits.len(), 2);
    let first = state.screen.selection.clone();
    state.transcript.retain_bodies(&HashSet::new());
    assert!(next_hit(&mut state, false).is_err());
    assert_eq!(state.screen.selection, first);
    assert_eq!(state.screen.search.current, Some(0));
    Ok(())
}

#[test]
fn missing_body_and_unknown_suffix_never_become_complete_no_match() -> TestResult {
    let (store, mut state) = fixture()?;
    state
        .transcript
        .config
        .set_body_demand(std::num::NonZeroUsize::new(2).ok_or("zero fixture demand")?);
    body(&mut state, &store, "first matching later")?;
    state
        .transcript
        .notice(ViewItemKind::Notice, "unloaded", Some("matching"))?;
    search(&mut state, SearchScope::LoadedTranscript, "matching")?;
    let report = state.screen.search.summary.ok_or("missing search report")?;
    assert_eq!(report.matches_found, 0);
    assert_eq!(report.partial_body_scans, 1);
    assert_eq!(report.unavailable_bodies, 1);
    assert!(!report.complete_within_scope());
    Ok(())
}

#[tokio::test]
async fn older_search_reads_exact_requested_page_and_reports_unloaded_suffixes() -> TestResult {
    let (store, mut state) = fixture()?;
    for number in 0..4 {
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: format!("needle {number}"),
        })?;
    }
    state
        .transcript
        .config
        .set_history_demand(std::num::NonZeroUsize::new(2).ok_or("zero fixture history")?);
    state
        .transcript
        .accept_history(&store.history_page(&state.transcript.initial_history()?)?)?;
    search(&mut state, SearchScope::RequestedOlderHistory, "needle")?;
    let mut pinned = HashSet::new();
    load_requests(&mut state, &store, &mut pinned)?;
    let result = state
        .transcript
        .history_tasks
        .join_next()
        .await
        .ok_or("history request was not scheduled")?;
    finish_history(&mut state, result)?;
    load_requests(&mut state, &store, &mut pinned)?;
    while let Some(result) = state.transcript.body_tasks.join_next().await {
        state.transcript.finish_body(result)?;
    }
    load_requests(&mut state, &store, &mut pinned)?;
    let summary = state
        .screen
        .search
        .summary
        .ok_or("older search did not complete")?;
    assert_eq!(summary.scope, SearchScope::RequestedOlderHistory);
    assert_eq!(summary.body_scans, 2);
    assert_eq!(summary.matches_found, 2);
    assert_eq!(summary.unavailable_bodies, 0);
    assert!(state.screen.search.older.is_none());
    Ok(())
}

#[tokio::test]
async fn export_keeps_exact_original_bytes_and_is_joined_after_view_rotation() -> TestResult {
    let (store, mut state) = fixture()?;
    let id = body(
        &mut state,
        &store,
        "**original**\nsoft wraps never enter\u{1b}",
    )?;
    state
        .screen
        .viewport
        .select(id, &state.transcript.projection)?;
    super::super::select_original(&mut state, 0, None)?;
    let expected = super::super::selected_text(&state)?.to_owned();
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("explicit export.txt");
    export(
        &mut state,
        destination.to_str().ok_or("fixture path UTF8")?,
        ExportMode::CreateNew,
    )?;
    let (_, replacement) = fixture()?;
    state.transcript = replacement.transcript;
    state
        .screen
        .replace_source(state.transcript.projection.source());
    assert_eq!(state.export_tasks.len(), 1);
    drain_exports(&mut state).await?;
    assert_eq!(std::fs::read_to_string(destination)?, expected);
    assert!(state.export_tasks.is_empty());
    assert!(
        state
            .screen
            .feedback
            .as_ref()
            .is_some_and(|value| value.contains("Exported"))
    );
    Ok(())
}
