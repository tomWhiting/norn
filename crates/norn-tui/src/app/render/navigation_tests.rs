//! Ordered scroll regressions use real projected bodies and the actual cached frame preparer.

use std::fmt::Write;

use super::*;
use crate::app::transcript::LoadedBody;
use norn::session::{EventStore, SessionBinding};
use norn::session_view::ViewItemKind;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture(content: &str, width: u16, rows: u16) -> TestResult<AppState> {
    let store = EventStore::new();
    let source = store.bind_view_source(
        &SessionBinding::ephemeral_root(),
        uuid::Uuid::new_v4(),
        None,
    )?;
    let mut state = AppState::new(
        crate::terminal::caps::TerminalCaps::baseline(),
        crate::input::history::InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        source,
        crate::render::fixed_panel::StatusBar::default(),
    );
    let id = state
        .transcript
        .notice(ViewItemKind::Input, "", Some(content))?;
    let reference = state
        .transcript
        .projection
        .item(&id)
        .and_then(|item| item.bodies.first())
        .ok_or("fixture body missing")?
        .clone();
    let demand = state
        .transcript
        .demand_body(&id, &reference, false)?
        .ok_or("fixture demand missing")?;
    let loaded: LoadedBody = state.transcript.read_local_body(&demand)?;
    state.transcript.accept_body(&demand, loaded)?;
    super::super::prepare(&mut state, width, rows)?;
    Ok(state)
}

fn numbered() -> TestResult<String> {
    let mut text = String::new();
    for index in 0..40 {
        writeln!(text, "line {index:02}")?;
    }
    Ok(text)
}

fn first_text(state: &AppState) -> TestResult<&str> {
    let hit = state
        .screen
        .hit_rows
        .first()
        .ok_or("first painted row missing")?;
    Ok(&hit.text.styled.text()[hit.geometry.bytes()])
}

fn body_offset(state: &AppState) -> TestResult<usize> {
    let anchor = state.screen.viewport.anchor().ok_or("body anchor absent")?;
    let AnchorPosition::Body {
        reference,
        original_offset,
    } = &anchor.position
    else {
        return Err("expected original body anchor".into());
    };
    let item = state
        .transcript
        .projection
        .item(&anchor.item)
        .ok_or("anchored item absent")?;
    assert!(
        item.bodies.contains(reference),
        "anchor must retain its own source capability"
    );
    Ok(*original_offset)
}

#[test]
fn same_direction_batch_keeps_every_row_and_direction_changes_keep_order() -> TestResult {
    let mut state = fixture(&numbered()?, 80, 14)?;
    let initial = first_text(&state)?.to_owned();
    for _ in 0..7 {
        queue(&mut state, true, 1)?;
    }
    assert_eq!(
        state
            .screen
            .navigation
            .as_ref()
            .ok_or("pending segment missing")?
            .motions
            .len(),
        1
    );
    assert_eq!(
        state
            .screen
            .navigation
            .as_ref()
            .ok_or("pending segment missing")?
            .motions[0]
            .rows,
        7
    );
    assert_eq!(
        first_text(&state)?,
        initial,
        "input queue must not publish intermediate frames"
    );
    super::super::prepare(&mut state, 80, 14)?;
    let after_seven = first_text(&state)?.to_owned();
    assert_ne!(after_seven, initial);
    let mut reference = fixture(&numbered()?, 80, 14)?;
    for _ in 0..7 {
        queue(&mut reference, true, 1)?;
        super::super::prepare(&mut reference, 80, 14)?;
    }
    assert_eq!(first_text(&state)?, first_text(&reference)?);
    queue(&mut state, true, 100)?;
    queue(&mut state, false, 3)?;
    assert_eq!(
        state
            .screen
            .navigation
            .as_ref()
            .ok_or("reversal missing")?
            .motions
            .len(),
        2
    );
    super::super::prepare(&mut state, 80, 14)?;
    assert_eq!(first_text(&state)?, "line 03");
    assert!(!state.screen.viewport.follows_tail());
    Ok(())
}

#[test]
fn large_cached_advance_does_not_materialize_travelled_paint_rows() -> TestResult {
    let mut state = fixture(&numbered()?, 80, 14)?;
    let previous_rows = state.screen.hit_rows.len();
    let reference = state
        .screen
        .hit_rows
        .first()
        .and_then(|hit| hit.body.clone())
        .ok_or("cached body missing")?;
    let text = Arc::clone(
        &state
            .screen
            .displayed
            .get(&reference)
            .ok_or("display cache missing")?
            .text,
    );
    queue(&mut state, true, usize::MAX)?;
    apply(&mut state)?;
    assert_eq!(
        state.screen.hit_rows.len(),
        previous_rows,
        "geometry traversal must not manufacture paint rows"
    );
    assert!(Arc::ptr_eq(
        &text,
        &state
            .screen
            .displayed
            .get(&reference)
            .ok_or("display cache evicted")?
            .text
    ));
    super::super::prepare(&mut state, 80, 14)?;
    assert_eq!(first_text(&state)?, "line 00");
    Ok(())
}

#[test]
fn transformed_control_rows_keep_exact_cached_display_position() -> TestResult {
    let mut state = fixture("\u{1b}\nend\n", 3, 8)?;
    queue(&mut state, true, 100)?;
    super::super::prepare(&mut state, 3, 8)?;
    let first = state
        .screen
        .viewport
        .anchor()
        .cloned()
        .ok_or("first anchor missing")?;
    let first_display = state
        .screen
        .row_cursor
        .as_ref()
        .ok_or("display cursor missing")?
        .display_start;
    queue(&mut state, false, 1)?;
    super::super::prepare(&mut state, 3, 8)?;
    let next = state
        .screen
        .viewport
        .anchor()
        .cloned()
        .ok_or("second anchor missing")?;
    assert_eq!(
        first.position, next.position,
        "escaped control rows share the real original start"
    );
    assert!(
        state
            .screen
            .row_cursor
            .as_ref()
            .ok_or("second display cursor missing")?
            .display_start
            > first_display
    );
    queue(&mut state, false, 1)?;
    super::super::prepare(&mut state, 3, 8)?;
    assert!(
        state
            .screen
            .row_cursor
            .as_ref()
            .ok_or("third display cursor missing")?
            .display_start
            > first_display + 1
    );
    Ok(())
}

#[test]
fn resize_is_an_ordered_barrier_and_source_replacement_discards_old_motion() -> TestResult {
    let content = "abcdefghijklmnopqrstuvwxyz\n".repeat(20);
    let mut batched = fixture(&content, 20, 14)?;
    let mut sequential = fixture(&content, 20, 14)?;
    queue(&mut batched, true, 3)?;
    queue(&mut sequential, true, 3)?;
    apply(&mut sequential)?;
    let old_offset = body_offset(&sequential)?;
    super::super::sync_input_area(&mut batched, 12, 10)?;
    super::super::sync_input_area(&mut sequential, 12, 10)?;
    assert_ne!(
        batched.transcript.projection.source(),
        sequential.transcript.projection.source()
    );
    assert_eq!(body_offset(&batched)?, old_offset);
    assert!(batched.screen.navigation.is_none());
    assert!(batched.screen.row_cursor.is_none());
    queue(&mut batched, false, 2)?;
    queue(&mut sequential, false, 2)?;
    super::super::prepare(&mut batched, 12, 10)?;
    super::super::prepare(&mut sequential, 12, 10)?;
    assert_eq!(first_text(&batched)?, first_text(&sequential)?);
    queue(&mut batched, true, 5)?;
    let source = crate::app::state::test_view_source(uuid::Uuid::new_v4());
    batched.screen.replace_source(&source);
    assert!(batched.screen.navigation.is_none());
    assert!(batched.screen.row_cursor.is_none());
    assert!(batched.screen.display_frame.is_none());
    Ok(())
}

#[test]
fn checked_batch_count_refuses_overflow_without_replacing_prior_motion() -> TestResult {
    let mut state = fixture(&numbered()?, 80, 14)?;
    queue(&mut state, true, usize::MAX)?;
    assert!(queue(&mut state, true, 1).is_err());
    assert_eq!(
        state
            .screen
            .navigation
            .as_ref()
            .ok_or("prior segment missing")?
            .motions[0]
            .rows,
        usize::MAX
    );
    apply(&mut state)?;
    super::super::prepare(&mut state, 80, 14)?;
    assert_eq!(first_text(&state)?, "line 00");
    Ok(())
}
