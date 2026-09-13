//! Recovery requires published matching geometry; repeated failures retain every draft owner.

use super::*;
use crate::app::state::test_view_source;
use crate::input::history::InputHistory;
use crate::render::layout::Layout;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn state() -> AppState {
    AppState::new(
        crate::terminal::caps::TerminalCaps::baseline(),
        InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        test_view_source(Uuid::new_v4()),
        crate::render::fixed_panel::StatusBar::default(),
    )
}

fn frame(area: Rect) -> Arc<Frame> {
    Arc::new(Frame {
        layout: Layout::ResizeRequired { area },
        rows: Vec::new(),
        composer: None,
        cursor: None,
    })
}

#[test]
fn unpainted_failed_and_replaced_frames_never_exchange_drafts() -> TestResult {
    let mut state = state();
    state.input_editor.paste_cells("rejected")?;
    let original = state.input_editor.snapshot()?;
    let saved = state.input_editor.detach_draft(&original)?;
    state.composer_recovery.retain_rejected(saved);
    state.input_editor.paste_cells("new draft")?;
    let next = state.input_editor.snapshot()?;
    let area = Rect {
        column: 0,
        row: 23,
        width: 80,
        height: 1,
    };
    let (_, hit) = prepare(&mut state, area)?.ok_or("recovery absent")?;
    assert!(!activate(&mut state, hit.column, hit.row));
    let failure = crate::TuiError::Io(std::io::Error::other("fixture flush failed"));
    assert!(
        crate::app::view_actions::latest::finish_publication(
            &mut state.screen,
            frame(area),
            Err(failure)
        )
        .is_err()
    );
    assert!(!activate(&mut state, hit.column, hit.row));
    state.input_editor.validate_snapshot(&next)?;
    prepare(&mut state, area)?.ok_or("recovery absent")?;
    crate::app::view_actions::latest::finish_publication(&mut state.screen, frame(area), Ok(()))?;
    state.screen.display_frame = Some(frame(area));
    assert!(!activate(&mut state, hit.column, hit.row));
    state.input_editor.validate_snapshot(&next)?;
    prepare(&mut state, area)?.ok_or("recovery absent")?;
    crate::app::view_actions::latest::finish_publication(&mut state.screen, frame(area), Ok(()))?;
    state
        .screen
        .replace_source(&test_view_source(Uuid::new_v4()));
    assert!(!activate(&mut state, hit.column, hit.row));
    state.input_editor.validate_snapshot(&next)?;
    Ok(())
}

#[test]
fn successive_failures_cycle_all_drafts_and_ignore_double_click_without_publication() -> TestResult
{
    let mut state = state();
    for text in ["first", "second"] {
        state.input_editor.paste_cells(text)?;
        let snapshot = state.input_editor.snapshot()?;
        let saved = state.input_editor.detach_draft(&snapshot)?;
        state.composer_recovery.retain_rejected(saved);
    }
    state.input_editor.paste_cells("third")?;
    let area = Rect {
        column: 0,
        row: 23,
        width: 80,
        height: 1,
    };
    for expected in ["first", "second", "third", "first"] {
        let (_, hit) = prepare(&mut state, area)?.ok_or("recovery absent")?;
        crate::app::view_actions::latest::finish_publication(
            &mut state.screen,
            frame(area),
            Ok(()),
        )?;
        assert!(!activate(&mut state, hit.column, hit.row - 1));
        assert!(activate(&mut state, hit.column, hit.row));
        assert_eq!(state.input_editor.text(), expected);
        assert!(!activate(&mut state, hit.column, hit.row));
        assert_eq!(state.composer_recovery.drafts.len(), 2);
        assert!(!state.input_editor.history_prev()?);
    }
    Ok(())
}
