//! Published identity, failed-frame, geometry and drag regressions for agent selection.

use super::*;
use crate::app::view_actions::latest::finish_publication;
use crate::render::layout::{Layout, UpperLayout, UpperPane};
use norn::session::{EventStore, SessionBinding};
use termina::event::Modifiers;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture() -> TestResult<(ScreenState, AgentHit)> {
    let store = EventStore::new();
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), None)?;
    let mut screen = ScreenState::new(source);
    screen.auxiliary = AuxiliaryPane::Agents;
    let hit = AgentHit {
        id: Uuid::new_v4(),
        area: Rect {
            column: 40,
            row: 2,
            width: 40,
            height: 1,
        },
    };
    Ok((screen, hit))
}

fn frame(hit: AgentHit) -> Arc<Frame> {
    Arc::new(Frame {
        layout: Layout::Ready {
            upper: UpperLayout::Single {
                pane: UpperPane::Changes,
                area: hit.area,
            },
            composer: Rect {
                column: 0,
                row: 10,
                width: 80,
                height: 5,
            },
        },
        rows: Vec::new(),
        composer: None,
        cursor: None,
    })
}

fn publish(screen: &mut ScreenState, hit: AgentHit) -> TestResult {
    screen.agent_pane.prepared = vec![hit];
    let frame = frame(hit);
    screen.layout = frame.layout;
    finish_publication(screen, frame, Ok(()))?;
    Ok(())
}

fn event(hit: AgentHit, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        column: hit.area.column,
        row: hit.area.row,
        kind,
        modifiers: Modifiers::NONE,
    }
}

fn down(screen: &mut ScreenState, hit: AgentHit) {
    assert_eq!(
        gesture(screen, event(hit, MouseEventKind::Down(MouseButton::Left))),
        None
    );
}

fn up(screen: &mut ScreenState, hit: AgentHit) -> Option<Uuid> {
    gesture(screen, event(hit, MouseEventKind::Up(MouseButton::Left)))
}

#[test]
fn preparation_and_failed_flush_never_authorize_a_click() -> TestResult {
    let (mut screen, hit) = fixture()?;
    screen.agent_pane.prepared = vec![hit];
    down(&mut screen, hit);
    assert_eq!(up(&mut screen, hit), None);
    publish(&mut screen, hit)?;
    down(&mut screen, hit);
    let error = super::super::render::interaction(std::io::Error::other("fixture flush failure"));
    assert!(finish_publication(&mut screen, frame(hit), Err(error)).is_err());
    assert_eq!(up(&mut screen, hit), None);
    publish(&mut screen, hit)?;
    down(&mut screen, hit);
    assert_eq!(up(&mut screen, hit), Some(hit.id));
    assert_eq!(up(&mut screen, hit), None);
    Ok(())
}

#[test]
fn status_refresh_preserves_identity_but_replacement_cannot_redirect_release() -> TestResult {
    let (mut screen, hit) = fixture()?;
    publish(&mut screen, hit)?;
    down(&mut screen, hit);
    publish(&mut screen, hit)?;
    assert_eq!(up(&mut screen, hit), Some(hit.id));
    down(&mut screen, hit);
    let replacement = AgentHit {
        id: Uuid::new_v4(),
        ..hit
    };
    publish(&mut screen, replacement)?;
    assert_eq!(up(&mut screen, replacement), None);
    Ok(())
}

#[test]
fn drag_and_second_press_on_pinned_text_do_not_open_agents() -> TestResult {
    let (mut screen, hit) = fixture()?;
    publish(&mut screen, hit)?;
    down(&mut screen, hit);
    gesture(
        &mut screen,
        event(hit, MouseEventKind::Drag(MouseButton::Left)),
    );
    assert_eq!(up(&mut screen, hit), None);
    screen.dragging_selection = true;
    down(&mut screen, hit);
    assert_eq!(up(&mut screen, hit), None);
    Ok(())
}

#[test]
fn resize_pane_switch_and_source_rotation_revoke_the_old_press() -> TestResult {
    let (mut screen, hit) = fixture()?;
    publish(&mut screen, hit)?;
    down(&mut screen, hit);
    crate::app::display_selection::sync_geometry(&mut screen, 81, 15);
    assert_eq!(up(&mut screen, hit), None);
    publish(&mut screen, hit)?;
    down(&mut screen, hit);
    screen.auxiliary = AuxiliaryPane::Diff;
    assert_eq!(up(&mut screen, hit), None);
    screen.auxiliary = AuxiliaryPane::Agents;
    publish(&mut screen, hit)?;
    down(&mut screen, hit);
    let store = EventStore::new();
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), None)?;
    screen.replace_source(&source);
    assert_eq!(up(&mut screen, hit), None);
    Ok(())
}
