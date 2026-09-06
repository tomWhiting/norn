//! Display snapshots preserve visible graphemes, generated rows and immutable gesture bytes.

use std::num::NonZeroUsize;

use norn::session_view::SessionIdentity;
use uuid::Uuid;

use super::{DisplayPane, DisplaySelection, paint};
use crate::app::render::ScreenState;
use crate::render::frame::{Frame, PaintRow};
use crate::render::layout::UpperPane;
use crate::render::layout::{Layout, Rect, UpperLayout};
use crate::render::retained_markdown::render_plain;
use crate::render::retained_text::TextLayout;
use norn::session_view::ViewSource;
use std::sync::Arc;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn unique_frame(frame: Arc<Frame>) -> Result<Frame, Box<dyn std::error::Error>> {
    Arc::try_unwrap(frame).map_err(|frame| {
        format!(
            "fixture frame still has {} owners",
            Arc::strong_count(&frame)
        )
        .into()
    })
}

fn source() -> ViewSource {
    ViewSource {
        session: SessionIdentity::Ephemeral(Uuid::new_v4()),
        agent_id: Uuid::new_v4(),
        parent_agent_id: None,
        store_generation: Uuid::new_v4(),
    }
}

fn fixture(text: &str, width: u16) -> Result<(Arc<Frame>, Rect), Box<dyn std::error::Error>> {
    let text = Arc::new(render_plain(text)?);
    let TextLayout::Rows(rows) = text
        .styled
        .layout(usize::from(width), NonZeroUsize::new(4).ok_or("tab width")?)?
    else {
        return Err("missing rows".into());
    };
    let area = Rect {
        column: 0,
        row: 0,
        width,
        height: u16::try_from(rows.len())?,
    };
    let paints = rows
        .into_iter()
        .enumerate()
        .map(|(row, geometry)| {
            Ok(PaintRow {
                area,
                row: u16::try_from(row)?,
                text: Arc::clone(&text),
                geometry,
                selected: false,
                selection: Vec::new(),
                composer: false,
            })
        })
        .collect::<Result<Vec<_>, std::num::TryFromIntError>>()?;
    Ok((
        Arc::new(Frame {
            layout: Layout::Ready {
                upper: UpperLayout::Single {
                    pane: UpperPane::Conversation,
                    area,
                },
                composer: Rect {
                    column: 0,
                    row: area.height,
                    width,
                    height: 1,
                },
            },
            rows: paints,
            composer: None,
            cursor: None,
        }),
        area,
    ))
}

#[test]
fn generated_and_cross_entry_text_is_display_scope_in_both_directions() -> TestResult {
    let source = source();
    let (frame, area) = fixture("Tool header\nbody text\nDiff · summary", 30)?;
    let mut selection = DisplaySelection::capture(
        source.clone(),
        Arc::clone(&frame),
        DisplayPane::Conversation(area),
        &[],
        0,
        0,
    )?;
    selection.extend(14, 2);
    assert_eq!(
        selection.text(&source)?,
        "Tool header\nbody text\nDiff · summary"
    );
    let mut reverse = DisplaySelection::capture(
        source.clone(),
        frame,
        DisplayPane::Conversation(area),
        &[],
        14,
        2,
    )?;
    reverse.extend(0, 0);
    assert_eq!(reverse.text(&source)?, selection.text(&source)?);
    assert!(
        selection.hit(0, 0).is_none(),
        "display scope cannot mint an original mapping"
    );
    assert!(selection.text(&self::source()).is_err());
    Ok(())
}

#[test]
fn wide_graphemes_are_whole_and_soft_wrap_is_explicit_display_text() -> TestResult {
    let source = source();
    let (frame, area) = fixture("e\u{301}👩‍💻界 tail", 6)?;
    let mut selection = DisplaySelection::capture(
        source.clone(),
        frame,
        DisplayPane::Conversation(area),
        &[],
        2,
        0,
    )?;
    selection.extend(4, 0);
    assert_eq!(selection.text(&source)?, "👩‍💻界");
    let (frame, area) = fixture("abcdef", 3)?;
    let mut selection = DisplaySelection::capture(
        source.clone(),
        frame,
        DisplayPane::Conversation(area),
        &[],
        0,
        0,
    )?;
    selection.extend(3, 1);
    assert_eq!(selection.text(&source)?, "abc\ndef");
    Ok(())
}

#[test]
fn stream_repaint_is_pinned_and_resize_ends_gesture_without_changing_copy() -> TestResult {
    let source = source();
    let (old, area) = fixture("old text", 20)?;
    let mut selected = DisplaySelection::capture(
        source.clone(),
        old,
        DisplayPane::Conversation(area),
        &[],
        0,
        0,
    )?;
    selected.extend(8, 0);
    let mut screen = ScreenState::new(source.clone());
    screen.display_selection = Some(selected);
    screen.dragging_selection = true;
    let (new, _) = fixture("new text", 20)?;
    let mut new = unique_frame(new)?;
    paint(&mut screen, &mut new)?;
    assert_eq!(new.rows[0].text.styled.text(), "old text");
    assert!(!new.rows[0].selection.is_empty());
    let (resized, _) = fixture("new text", 4)?;
    let mut resized = unique_frame(resized)?;
    paint(&mut screen, &mut resized)?;
    assert!(!screen.dragging_selection);
    assert_eq!(
        screen
            .display_selection
            .as_ref()
            .ok_or("selection missing")?
            .text(&source)?,
        "old text"
    );
    Ok(())
}

#[test]
fn displayed_controls_are_visible_escapes_and_tabs_are_displayed_spaces() -> TestResult {
    let source = source();
    let (frame, area) = fixture("a\tb\x1b[31m", 40)?;
    let mut selection = DisplaySelection::capture(
        source.clone(),
        frame,
        DisplayPane::Conversation(area),
        &[],
        0,
        0,
    )?;
    selection.extend(40, 0);
    let text = selection.text(&source)?;
    assert!(text.starts_with("a   b"));
    assert!(!text.contains('\x1b'));
    assert!(!text.contains('\t'));
    assert!(text.contains("31m"));
    Ok(())
}

#[test]
fn resize_revokes_mapping_before_queued_drag_and_release_can_change_copied_bytes() -> TestResult {
    use crate::app::render::sync_input_area;
    use crate::app::state::AppState;
    use crate::input::InputHistory;
    use crate::render::fixed_panel::StatusBar;
    use crate::terminal::caps::TerminalCaps;
    use termina::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};
    let source = source();
    let mut state = AppState::new(
        TerminalCaps::baseline(),
        InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        source.clone(),
        StatusBar::default(),
    );
    sync_input_area(&mut state, 20, 2)?;
    let (frame, area) = fixture("old text", 20)?;
    state.screen.layout = frame.layout;
    state.screen.display_frame = Some(Arc::clone(&frame));
    let mut selection = DisplaySelection::capture(
        source.clone(),
        frame,
        DisplayPane::Conversation(area),
        &[],
        0,
        0,
    )?;
    selection.extend(3, 0);
    state.screen.display_selection = Some(selection);
    state.screen.dragging_selection = true;
    // This is the same synchronous resize seam called by both actual loops. Its
    // queued terminal frontier defers paint; Drag/Up arrive before that paint.
    state.screen.terminal_event(2);
    sync_input_area(&mut state, 8, 2)?;
    assert!(
        !state.screen.dragging_selection,
        "resize waited for paint to cancel the old mapping"
    );
    for kind in [
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::app::view_actions::mouse(
            MouseEvent {
                kind,
                column: 7,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut state,
        );
    }
    assert_eq!(
        state
            .screen
            .display_selection
            .as_ref()
            .ok_or("snapshot lost")?
            .text(&source)?,
        "old"
    );
    Ok(())
}

#[test]
fn same_rectangle_auxiliary_switch_ends_drag_without_overlaying_the_previous_pane() -> TestResult {
    use crate::app::render::AuxiliaryPane;
    let source = source();
    let (old, area) = fixture("Diff bytes", 20)?;
    let mut old = unique_frame(old)?;
    if let Layout::Ready { upper, .. } = &mut old.layout {
        *upper = UpperLayout::Single {
            pane: UpperPane::Changes,
            area,
        };
    }
    let mut selection = DisplaySelection::capture(
        source.clone(),
        Arc::new(old),
        DisplayPane::Auxiliary(area, AuxiliaryPane::Diff),
        &[],
        0,
        0,
    )?;
    selection.extend(10, 0);
    let mut screen = ScreenState::new(source.clone());
    screen.display_selection = Some(selection);
    screen.dragging_selection = true;
    screen.changes_open = true;
    screen.upper = UpperPane::Changes;
    screen.auxiliary = AuxiliaryPane::Agents;
    let (current, _) = fixture("Agents bytes", 20)?;
    let mut current = unique_frame(current)?;
    if let Layout::Ready { upper, .. } = &mut current.layout {
        *upper = UpperLayout::Single {
            pane: UpperPane::Changes,
            area,
        };
    }
    paint(&mut screen, &mut current)?;
    assert!(!screen.dragging_selection);
    assert_eq!(current.rows[0].text.styled.text(), "Agents bytes");
    assert_eq!(
        screen
            .display_selection
            .as_ref()
            .ok_or("snapshot lost")?
            .text(&source)?,
        "Diff bytes"
    );
    Ok(())
}

#[test]
fn long_selection_merges_adjacent_ranges_and_overlay_copy_uses_the_visible_glyphs() -> TestResult {
    let source = source();
    let (frame, area) = fixture(&"a".repeat(160), 200)?;
    let mut selection = DisplaySelection::capture(
        source.clone(),
        Arc::clone(&frame),
        DisplayPane::Conversation(area),
        &[],
        0,
        0,
    )?;
    selection.extend(160, 0);
    let mut screen = ScreenState::new(source.clone());
    screen.display_selection = Some(selection);
    screen.dragging_selection = true;
    let (current, _) = fixture("new", 200)?;
    let mut current = unique_frame(current)?;
    paint(&mut screen, &mut current)?;
    assert_eq!(
        current.rows[0].selection,
        vec![0..160],
        "one range per glyph makes each frame scan quadratic"
    );

    let (base, area) = fixture("abcdef", 20)?;
    let mut base = unique_frame(base)?;
    let (overlay, _) = fixture("TOP", 3)?;
    let mut overlay = unique_frame(overlay)?;
    let mut row = overlay.rows.pop().ok_or("missing overlay")?;
    row.area.column = 2;
    base.rows.push(row);
    let mut selection = DisplaySelection::capture(
        source.clone(),
        Arc::new(base),
        DisplayPane::Conversation(area),
        &[],
        0,
        0,
    )?;
    selection.extend(6, 0);
    assert_eq!(selection.text(&source)?, "abTOPf");
    Ok(())
}
