//! Published reading snapshots preserve retired display bytes without reviving source authority.

use super::*;
use crate::app::display_selection::{DisplayPane, DisplaySelection};
use crate::app::view_actions;
use crate::render::retained_text::TextAttribute;
use norn::provider::agent_event::{AgentEvent, AgentEventKind};
use norn::provider::events::ProviderEvent;
use norn::session::{EventStore, SessionBinding};
use norn::session_view::{AcceptedModel, DisplayText};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn delta(state: &mut AppState, text: &str) -> TestResult {
    state.transcript.apply_live(&AgentEvent {
        agent_id: state.transcript.projection.source().agent_id,
        agent_role: Arc::from("reading-test"),
        event: AgentEventKind::Provider(ProviderEvent::TextDelta {
            text: text.to_owned(),
        }),
    })?;
    Ok(())
}

fn fixture() -> TestResult<AppState> {
    let source = EventStore::new().bind_view_source(
        &SessionBinding::ephemeral_root(),
        Uuid::new_v4(),
        None,
    )?;
    let mut state = AppState::new(
        crate::terminal::caps::TerminalCaps::baseline(),
        crate::input::history::InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        source,
        crate::render::fixed_panel::StatusBar::default(),
    );
    state.transcript.projection.begin_execution(
        Uuid::new_v4(),
        AcceptedModel {
            model: DisplayText::new("reading-test"),
            backend: None,
            context_window: 4096,
            effort: None,
            tier: None,
            configuration_revision: 1,
        },
    )?;
    delta(
        &mut state,
        "unseen first line\n**retained 界 e\u{301} words across wrap**",
    )?;
    super::super::prepare(&mut state, 20, 6)?;
    for (item, reference) in state.screen.demands.clone() {
        let demand = state
            .transcript
            .demand_body(&item, &reference, false)?
            .ok_or("body demand")?;
        let page = state.transcript.read_local_body(&demand)?;
        state.transcript.accept_body(&demand, page)?;
    }
    let frame = super::super::prepare(&mut state, 20, 6)?;
    view_actions::latest::finish_publication(&mut state.screen, Arc::new(frame), Ok(()))?;
    view_actions::pin_visible(&mut state)?;
    Ok(state)
}

fn displayed(frame: &Frame, area: Rect) -> String {
    frame
        .rows
        .iter()
        .filter(|paint| paint.area.row + paint.row < area.row + area.height)
        .map(|paint| &paint.text.styled.text()[paint.geometry.bytes()])
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn retired_revision_reflows_visible_bytes_and_keeps_copy_scopes_distinct() -> TestResult {
    let mut state = fixture()?;
    let anchor = state.screen.viewport.anchor().cloned().ok_or("anchor")?;
    assert!(matches!(anchor.position, AnchorPosition::Body { .. }));
    state
        .screen
        .viewport
        .select(anchor.item.clone(), &state.transcript.projection)?;
    view_actions::command("select 0 20 28", &mut state)?;
    assert_eq!(view_actions::selected_text(&state)?, "retained");
    let selected_frame = super::super::prepare(&mut state, 20, 7)?;
    view_actions::latest::finish_publication(&mut state.screen, Arc::new(selected_frame), Ok(()))?;
    delta(&mut state, " NEW REVISION")?;
    let frame = super::super::prepare(&mut state, 40, 12)?;
    let area = Rect {
        row: 0,
        column: 0,
        width: 40,
        height: 8,
    };
    let old = displayed(&frame, area);
    assert!(
        !old.contains("unseen"),
        "offscreen bytes must not be invented: {old}"
    );
    assert!(!old.contains("NEW REVISION"));
    assert!(
        old.contains("retained 界 e\u{301} words across wrap"),
        "{old}"
    );
    assert_eq!(state.screen.viewport.anchor(), Some(&anchor));
    assert!(view_actions::selected_text(&state).is_err());
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.selected && !row.selection.is_empty())
    );
    assert!(state.screen.hit_rows.is_empty());
    assert!(state.screen.demands.is_empty());
    assert!(frame.rows.iter().any(|paint| {
        paint
            .text
            .styled
            .spans()
            .iter()
            .any(|span| span.style.attributes.contains(TextAttribute::Bold))
    }));
    let frame = Arc::new(frame);
    let mut selection = DisplaySelection::capture(
        state.transcript.projection.source().clone(),
        Arc::clone(&frame),
        DisplayPane::Conversation(area),
        &state.screen.hit_rows,
        0,
        0,
    )?;
    selection.extend(39, 0);
    assert_eq!(
        selection.text(state.transcript.projection.source())?,
        "retained 界 e\u{301} words across wrap"
    );
    let narrow = super::super::prepare(&mut state, 8, 24)?;
    assert!(
        !displayed(
            &narrow,
            Rect {
                width: 8,
                height: 20,
                ..area
            }
        )
        .contains("NEW")
    );
    let wide_again = super::super::prepare(&mut state, 40, 12)?;
    assert_eq!(displayed(&wide_again, area), old);
    assert_eq!(state.screen.viewport.anchor(), Some(&anchor));
    view_actions::latest::follow_latest(&mut state);
    assert!(state.screen.reading_snapshot.is_none());
    assert!(state.screen.viewport.follows_tail());
    Ok(())
}

#[test]
fn failed_publication_cannot_replace_snapshot_and_source_change_retires_it() -> TestResult {
    let mut state = fixture()?;
    let before = state
        .screen
        .reading_snapshot
        .as_ref()
        .ok_or("snapshot")?
        .anchor
        .clone();
    let frame = super::super::prepare(&mut state, 40, 12)?;
    let failure = super::super::interaction(std::io::Error::other("fixture flush failed"));
    assert!(
        view_actions::latest::finish_publication(&mut state.screen, Arc::new(frame), Err(failure))
            .is_err()
    );
    assert_eq!(
        state
            .screen
            .reading_snapshot
            .as_ref()
            .ok_or("snapshot")?
            .anchor,
        before
    );
    assert!(state.screen.display_frame.is_none());
    assert!(state.screen.prepared_reading.is_none());
    let source = EventStore::new().bind_view_source(
        &SessionBinding::ephemeral_root(),
        Uuid::new_v4(),
        None,
    )?;
    state.screen.replace_source(&source);
    assert!(state.screen.reading_snapshot.is_none());
    Ok(())
}
