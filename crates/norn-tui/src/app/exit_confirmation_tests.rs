//! Confirmation boundaries are deterministic; no sleeps or live terminal are required.

use super::*;

#[test]
fn second_press_inside_window_confirms_and_disarms() {
    let now = Instant::now();
    let mut confirmation = ExitConfirmation::default();
    assert!(!confirmation.press(now));
    assert_eq!(confirmation.hint(), "Press Ctrl+C again within 3s to exit");
    assert!(confirmation.press(now + Duration::from_secs(2)));
    assert!(confirmation.requested());
    assert_eq!(
        confirmation.hint(),
        "Exiting; waiting for cancelled work to settle"
    );
}

#[test]
fn expired_press_arms_a_new_window() {
    let now = Instant::now();
    let mut confirmation = ExitConfirmation::default();
    assert!(!confirmation.press(now));
    assert!(!confirmation.press(now + CONFIRM_WINDOW));
    assert!(confirmation.press(now + CONFIRM_WINDOW + Duration::from_millis(1)));
}

#[test]
fn unrelated_input_clears_confirmation() {
    let now = Instant::now();
    let mut confirmation = ExitConfirmation::default();
    assert!(!confirmation.press(now));
    assert!(confirmation.clear());
    assert!(!confirmation.clear());
    assert!(!confirmation.press(now + Duration::from_millis(1)));
}

#[test]
fn tick_clears_hint_at_exact_deadline() {
    let now = Instant::now();
    let mut confirmation = ExitConfirmation::default();
    assert!(!confirmation.press(now));
    assert!(!confirmation.expire(now + Duration::from_secs(2)));
    assert!(confirmation.expire(now + CONFIRM_WINDOW));
    assert_eq!(confirmation.hint(), "^C twice to exit");
    assert!(!confirmation.expire(now + CONFIRM_WINDOW));
}

#[test]
fn active_cancel_preserves_confirmation_across_turn_completion() {
    let now = Instant::now();
    let root = tokio_util::sync::CancellationToken::new();
    let turn = root.child_token();
    let sibling = root.child_token();
    let mut confirmation = ExitConfirmation::default();
    confirmation.interrupt(now, &turn, &root);
    assert!(turn.is_cancelled());
    assert!(!root.is_cancelled());
    assert!(!sibling.is_cancelled());
    assert!(confirmation.blocks_automatic_work());
    assert!(!confirmation.requested());
    assert!(confirmation.press(now + Duration::from_millis(100)));
    assert!(confirmation.requested());
    assert!(confirmation.blocks_automatic_work());
}

#[test]
fn second_press_while_settling_cancels_descendants_and_keeps_exit_requested() {
    let now = Instant::now();
    let root = tokio_util::sync::CancellationToken::new();
    let turn = root.child_token();
    let sibling = root.child_token();
    let mut confirmation = ExitConfirmation::default();
    confirmation.interrupt(now, &turn, &root);
    confirmation.interrupt(now + Duration::from_millis(100), &turn, &root);
    assert!(root.is_cancelled());
    assert!(sibling.is_cancelled());
    assert!(confirmation.requested());
    confirmation.clear();
    confirmation.expire(now + CONFIRM_WINDOW);
    assert!(confirmation.blocks_automatic_work());
    assert!(confirmation.requested());
}

#[test]
fn elapsed_confirmation_does_not_turn_a_later_cancel_into_exit() {
    let now = Instant::now();
    let root = tokio_util::sync::CancellationToken::new();
    let turn = root.child_token();
    let mut confirmation = ExitConfirmation::default();
    confirmation.interrupt(now, &turn, &root);
    confirmation.interrupt(now + CONFIRM_WINDOW, &turn, &root);
    assert!(!root.is_cancelled());
    assert!(!confirmation.requested());
    assert!(confirmation.blocks_automatic_work());
}

#[test]
fn real_non_cancel_input_disarms_but_release_and_resize_do_not() {
    use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
    use termina::{Event, WindowSize};
    let now = Instant::now();
    let mut confirmation = ExitConfirmation::default();
    assert!(!confirmation.press(now));
    let mut release = KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL);
    release.kind = KeyEventKind::Release;
    assert!(!confirmation.observe_input(&Event::Key(release)));
    assert!(confirmation.is_armed());
    assert!(
        !confirmation.observe_input(&Event::WindowResized(WindowSize {
            cols: 80,
            rows: 24,
            pixel_width: None,
            pixel_height: None
        }))
    );
    assert!(confirmation.is_armed());
    assert!(confirmation.observe_input(&Event::Key(KeyEvent::new(KeyCode::Left, Modifiers::NONE))));
    assert!(!confirmation.is_armed());
    assert!(!confirmation.press(now));
    assert!(confirmation.observe_input(&Event::Paste("draft".to_owned())));
    assert!(!confirmation.is_armed());
}
