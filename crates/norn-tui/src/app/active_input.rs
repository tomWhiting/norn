//! TUI state for in-flight human input.
//!
//! While a root turn is running, Enter submits non-empty composer text as a
//! steer by default. The user can toggle to queue mode, and a blank Enter with
//! pending steers requests an interrupt so those steers can be submitted as a
//! fresh turn immediately.

use std::collections::VecDeque;

use uuid::Uuid;

/// How Enter treats non-empty composer text while a turn is running.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InFlightSubmitMode {
    /// Submit text to the active turn's steer queue.
    Steer,
    /// Hold text for the next normal user turn.
    Queue,
}

impl InFlightSubmitMode {
    /// Stable short label for panel hints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::Queue => "queue",
        }
    }

    const fn toggled(self) -> Self {
        match self {
            Self::Steer => Self::Queue,
            Self::Queue => Self::Steer,
        }
    }
}

/// One steer accepted by the active turn but not yet persisted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingSteer {
    /// Active-input id returned by the core channel.
    pub id: Uuid,
    /// User-authored text.
    pub content: String,
}

/// Mutable in-flight input state owned by [`super::state::AppState`].
#[derive(Debug)]
pub struct InFlightInputState {
    mode: InFlightSubmitMode,
    running: bool,
    pending_steers: VecDeque<PendingSteer>,
    queued_followups: VecDeque<String>,
    submit_pending_steers_after_interrupt: bool,
}

impl Default for InFlightInputState {
    fn default() -> Self {
        Self {
            mode: InFlightSubmitMode::Steer,
            running: false,
            pending_steers: VecDeque::new(),
            queued_followups: VecDeque::new(),
            submit_pending_steers_after_interrupt: false,
        }
    }
}

impl InFlightInputState {
    /// Current in-flight submit mode.
    #[must_use]
    pub const fn mode(&self) -> InFlightSubmitMode {
        self.mode
    }

    /// Whether a root turn is currently running.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running
    }

    /// Enter or leave active-turn mode.
    pub fn set_running(&mut self, running: bool) {
        self.running = running;
        if !running {
            self.submit_pending_steers_after_interrupt = false;
            self.pending_steers.clear();
        }
    }

    /// Toggle non-empty Enter behavior between steer and queue modes.
    pub fn toggle_mode(&mut self) {
        self.mode = self.mode.toggled();
    }

    /// Track a steer accepted by the core active-turn channel.
    pub fn push_pending_steer(&mut self, id: Uuid, content: String) {
        self.pending_steers.push_back(PendingSteer { id, content });
    }

    /// Remove a steer once the runner reports it has been persisted.
    pub fn mark_steer_delivered(&mut self, id: Uuid) {
        if let Some(idx) = self.pending_steers.iter().position(|steer| steer.id == id) {
            self.pending_steers.remove(idx);
        }
    }

    /// Hold text for the next normal turn.
    pub fn queue_followup(&mut self, content: String) {
        self.queued_followups.push_back(content);
    }

    /// Pop the next queued normal turn.
    pub fn pop_queued_followup(&mut self) -> Option<String> {
        self.queued_followups.pop_front()
    }

    /// Whether any steers are awaiting delivery.
    #[must_use]
    pub fn has_pending_steers(&self) -> bool {
        !self.pending_steers.is_empty()
    }

    /// Mark that the current interrupt should immediately submit pending steers.
    pub fn request_interrupt_submit(&mut self) {
        if self.has_pending_steers() {
            self.submit_pending_steers_after_interrupt = true;
        }
    }

    /// Drain pending steers as a single prompt when interrupt-and-submit fired.
    pub fn take_interrupt_prompt(&mut self) -> Option<String> {
        if !self.submit_pending_steers_after_interrupt {
            return None;
        }
        self.submit_pending_steers_after_interrupt = false;
        let mut parts = Vec::new();
        while let Some(steer) = self.pending_steers.pop_front() {
            parts.push(steer.content);
        }
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    /// Preserve accepted-but-undelivered steers as normal queued turns.
    ///
    /// This is the natural-completion race handoff: the core channel may accept
    /// input just before the running loop resolves, leaving no safe provider
    /// boundary to deliver it. Requeueing keeps operator input from vanishing
    /// while still avoiding false "delivered" UI state.
    pub fn requeue_pending_steers(&mut self) {
        self.submit_pending_steers_after_interrupt = false;
        while let Some(steer) = self.pending_steers.pop_front() {
            self.queued_followups.push_back(steer.content);
        }
    }

    /// Human-readable single-line status for the fixed panel.
    #[must_use]
    pub fn status_line(&self) -> Option<String> {
        if !self.running && self.pending_steers.is_empty() && self.queued_followups.is_empty() {
            return None;
        }
        let mut parts = vec![format!("mode:{}", self.mode.label())];
        if !self.pending_steers.is_empty() {
            let pending_steers = self.pending_steers.len();
            parts.push(format!("{pending_steers} steer pending"));
        }
        if !self.queued_followups.is_empty() {
            let queued_followups = self.queued_followups.len();
            parts.push(format!("{queued_followups} queued"));
        }
        let action = match (self.mode, self.pending_steers.is_empty()) {
            (InFlightSubmitMode::Steer, false) => "Enter text steers; blank Enter interrupts now",
            (InFlightSubmitMode::Steer, true) => "Enter text steers",
            (InFlightSubmitMode::Queue, _) => "Enter text queues next turn",
        };
        parts.push(action.to_string());
        parts.push("^T toggles".to_string());
        Some(parts.join(" | "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupt_prompt_drains_pending_steers_in_order() {
        let mut state = InFlightInputState::default();
        state.push_pending_steer(Uuid::from_u128(1), "first".to_string());
        state.push_pending_steer(Uuid::from_u128(2), "second".to_string());
        state.request_interrupt_submit();

        assert_eq!(
            state.take_interrupt_prompt(),
            Some("first\nsecond".to_string()),
        );
        assert!(!state.has_pending_steers());
    }

    #[test]
    fn delivered_steer_is_removed_from_preview() {
        let mut state = InFlightInputState::default();
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        state.push_pending_steer(first, "first".to_string());
        state.push_pending_steer(second, "second".to_string());

        state.mark_steer_delivered(first);

        assert_eq!(
            state.take_interrupt_prompt(),
            None,
            "delivery alone must not arm interrupt submit",
        );
        state.request_interrupt_submit();
        assert_eq!(state.take_interrupt_prompt(), Some("second".to_string()));
    }

    #[test]
    fn undelivered_steers_requeue_as_followups() {
        let mut state = InFlightInputState::default();
        state.push_pending_steer(Uuid::from_u128(1), "late steer".to_string());

        state.requeue_pending_steers();

        assert!(!state.has_pending_steers());
        assert_eq!(state.pop_queued_followup(), Some("late steer".to_string()));
    }
}
