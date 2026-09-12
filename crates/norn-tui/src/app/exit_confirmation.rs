//! Idle Ctrl+C confirmation uses monotonic time and never changes turn cancellation.

use std::time::{Duration, Instant};

/// A second distinct idle Ctrl+C press must arrive within this window.
const CONFIRM_WINDOW: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct ExitConfirmation {
    armed_at: Option<Instant>,
}

impl ExitConfirmation {
    /// Confirm a live request, otherwise start a fresh confirmation window.
    pub(super) fn press(&mut self, now: Instant) -> bool {
        if self
            .armed_at
            .take()
            .is_some_and(|at| now.saturating_duration_since(at) < CONFIRM_WINDOW)
        {
            true
        } else {
            self.armed_at = Some(now);
            false
        }
    }

    /// Returns whether a visible confirmation was cleared.
    pub(super) fn clear(&mut self) -> bool {
        self.armed_at.take().is_some()
    }

    /// The existing render tick retires the hint without a timer or task of its own.
    pub(super) fn expire(&mut self, now: Instant) -> bool {
        if self
            .armed_at
            .is_some_and(|at| now.saturating_duration_since(at) >= CONFIRM_WINDOW)
        {
            self.clear()
        } else {
            false
        }
    }

    pub(super) fn is_armed(&self) -> bool {
        self.armed_at.is_some()
    }

    pub(super) fn hint(&self) -> &'static str {
        if self.armed_at.is_some() {
            "Press Ctrl+C again within 3s to exit"
        } else {
            "^C twice to exit"
        }
    }
}

#[cfg(test)]
#[path = "exit_confirmation_tests.rs"]
mod tests;
