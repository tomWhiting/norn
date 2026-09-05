//! Declared frontend demands and preferences; these are not retention or runtime limits.

use std::num::NonZeroUsize;

use crate::TuiError;

/// Initial event demand, preserving the existing twenty-event replay policy.
pub const DEFAULT_HISTORY_EVENTS: usize = 20;
/// Original body bytes requested per explicit load, settled by NRT D-F4.
pub const DEFAULT_BODY_BYTES: usize = 65_536;

/// Frontend-local preferences; changing them cannot alter an agent request.
#[derive(Clone, Debug)]
pub struct ViewConfig {
    history_events: usize,
    body_bytes: usize,
    /// Default tool detail state; an item's explicit override takes precedence.
    pub expanded_tools: bool,
    /// Operator-selected clipboard transport; never inferred from the environment.
    pub(crate) clipboard: crate::terminal::clipboard::ClipboardCapability,
}

impl Default for ViewConfig {
    fn default() -> Self {
        Self {
            history_events: DEFAULT_HISTORY_EVENTS,
            body_bytes: DEFAULT_BODY_BYTES,
            expanded_tools: false,
            clipboard: crate::terminal::clipboard::ClipboardCapability::Unspecified,
        }
    }
}

impl ViewConfig {
    /// Current explicit positive event demand.
    pub fn history_demand(&self) -> Result<NonZeroUsize, TuiError> {
        NonZeroUsize::new(self.history_events).ok_or(TuiError::InvalidViewDemand {
            name: "history events",
            value: self.history_events,
        })
    }

    /// Current explicit positive original-byte demand.
    pub fn body_demand(&self) -> Result<NonZeroUsize, TuiError> {
        NonZeroUsize::new(self.body_bytes).ok_or(TuiError::InvalidViewDemand {
            name: "body bytes",
            value: self.body_bytes,
        })
    }

    /// Override the event demand with a validated positive value.
    pub fn set_history_demand(&mut self, demand: NonZeroUsize) {
        self.history_events = demand.get();
    }

    /// Override the body demand with a validated positive value.
    pub fn set_body_demand(&mut self, demand: NonZeroUsize) {
        self.body_bytes = demand.get();
    }
}

#[cfg(test)]
#[path = "view_config_tests.rs"]
mod tests;
