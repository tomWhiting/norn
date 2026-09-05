//! Explicit focus navigation over visible panes; activity and resize cannot steal focus.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Focus {
    Composer,
    Conversation,
    Changes,
    Divider,
}

const ORDER: [Focus; 4] = [
    Focus::Composer,
    Focus::Conversation,
    Focus::Changes,
    Focus::Divider,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FocusAvailability {
    pub composer: bool,
    pub conversation: bool,
    pub changes: bool,
    pub divider: bool,
}

impl FocusAvailability {
    pub const fn contains(self, focus: Focus) -> bool {
        match focus {
            Focus::Composer => self.composer,
            Focus::Conversation => self.conversation,
            Focus::Changes => self.changes,
            Focus::Divider => self.divider,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FocusDirection {
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum FocusError {
    #[error("no focus region is visible at the current geometry")]
    NoVisiblePane,
    #[error("focus region {target:?} is not visible at the current geometry")]
    Unavailable { target: Focus },
}

/// Requested focus survives hidden geometry; only explicit user actions replace it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FocusState {
    requested: Focus,
}

impl FocusState {
    pub const fn new() -> Self {
        Self {
            requested: Focus::Composer,
        }
    }

    pub const fn requested(self) -> Focus {
        self.requested
    }

    /// Resolve a visible target without changing the saved focus used after widening.
    pub fn visible(self, available: FocusAvailability) -> Result<Focus, FocusError> {
        if available.contains(self.requested) {
            return Ok(self.requested);
        }
        // An upper-pane focus follows the visible upper pane in narrow mode.
        let candidates = match self.requested {
            Focus::Composer => ORDER,
            Focus::Conversation => [
                Focus::Changes,
                Focus::Composer,
                Focus::Divider,
                Focus::Conversation,
            ],
            Focus::Changes | Focus::Divider => [
                Focus::Conversation,
                Focus::Changes,
                Focus::Composer,
                Focus::Divider,
            ],
        };
        candidates
            .into_iter()
            .find(|focus| available.contains(*focus))
            .ok_or(FocusError::NoVisiblePane)
    }

    pub fn focus(&mut self, target: Focus, available: FocusAvailability) -> Result<(), FocusError> {
        if !available.contains(target) {
            return Err(FocusError::Unavailable { target });
        }
        self.requested = target;
        Ok(())
    }

    /// F6/Shift+F6 cycle only currently visible controls, in settled pane order.
    pub fn cycle(
        &mut self,
        direction: FocusDirection,
        available: FocusAvailability,
    ) -> Result<Focus, FocusError> {
        let current = self.visible(available)?;
        let start = match current {
            Focus::Composer => 0,
            Focus::Conversation => 1,
            Focus::Changes => 2,
            Focus::Divider => 3,
        };
        for step in 1..=ORDER.len() {
            let index = match direction {
                FocusDirection::Forward => (start + step) % ORDER.len(),
                FocusDirection::Backward => (start + ORDER.len() - step) % ORDER.len(),
            };
            let candidate = ORDER[index];
            if available.contains(candidate) {
                self.requested = candidate;
                return Ok(candidate);
            }
        }
        Err(FocusError::NoVisiblePane)
    }
}
