//! Operator-owned exit intent survives automatic work and uses one monotonic confirmation window.

use std::time::{Duration, Instant};

/// A second distinct Ctrl+C press must arrive within this window.
const CONFIRM_WINDOW: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct ExitConfirmation {
    armed_at: Option<Instant>,
    requested: bool,
}

impl ExitConfirmation {
    /// Confirm a live request, otherwise start a fresh confirmation window.
    pub(super) fn press(&mut self, now: Instant) -> bool {
        if self
            .armed_at
            .take()
            .is_some_and(|at| now.saturating_duration_since(at) < CONFIRM_WINDOW)
        {
            self.requested = true;
            true
        } else {
            self.armed_at = Some(now);
            false
        }
    }

    /// First press cancels this turn; confirmation cancels the whole run tree.
    pub(super) fn interrupt(
        &mut self,
        now: Instant,
        turn: &tokio_util::sync::CancellationToken,
        root: &tokio_util::sync::CancellationToken,
    ) {
        turn.cancel();
        if self.press(now) {
            root.cancel();
        }
    }

    pub(super) fn requested(&self) -> bool {
        self.requested
    }

    /// Accepted MCP changes must settle before their waiter can be released.
    pub(super) fn ready_to_exit(&self, mcp_pending: bool) -> bool {
        self.requested && !mcp_pending
    }

    /// Background work must not consume the operator's chance to confirm exit.
    pub(super) fn blocks_automatic_work(&self) -> bool {
        self.requested || self.is_armed()
    }

    /// Only deliberate non-cancel input clears confirmation; background traffic never does.
    pub(super) fn observe_input(&mut self, event: &termina::Event) -> bool {
        use termina::event::{KeyCode, KeyEventKind, Modifiers};
        let clears = matches!(event, termina::Event::Paste(_))
            || matches!(event, termina::Event::Key(key) if key.kind != KeyEventKind::Release
                && !(key.code == KeyCode::Char('c') && key.modifiers.contains(Modifiers::CONTROL)));
        clears && self.clear()
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
        if self.requested {
            "Exiting; waiting for cancelled work to settle"
        } else if self.armed_at.is_some() {
            "Press Ctrl+C again within 3s to exit"
        } else {
            "^C twice to exit"
        }
    }
}

#[cfg(test)]
#[path = "exit_confirmation_tests.rs"]
mod tests;
