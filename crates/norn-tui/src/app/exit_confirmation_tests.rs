//! Confirmation boundaries are deterministic; no sleeps or live terminal are required.

use super::*;

#[test]
fn second_press_inside_window_confirms_and_disarms() {
    let now = Instant::now();
    let mut confirmation = ExitConfirmation::default();
    assert!(!confirmation.press(now));
    assert_eq!(confirmation.hint(), "Press Ctrl+C again within 3s to exit");
    assert!(confirmation.press(now + Duration::from_secs(2)));
    assert_eq!(confirmation.hint(), "^C twice to exit");
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
