//! Visibility state for optional event-rendering detail.

/// Visibility toggles for rendering output.
///
/// Thinking is visible by default so provider reasoning summaries are not
/// silently dropped. Secondary structured fields start hidden to keep the
/// default transcript compact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayToggles {
    /// Whether thinking content is rendered into the scroll region.
    pub thinking_visible: bool,
    /// Whether secondary structured-output fields are rendered.
    pub secondary_fields_visible: bool,
}

impl Default for DisplayToggles {
    fn default() -> Self {
        Self {
            thinking_visible: true,
            secondary_fields_visible: false,
        }
    }
}

impl DisplayToggles {
    /// Construct with thinking visible and secondary fields hidden.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            thinking_visible: true,
            secondary_fields_visible: false,
        }
    }

    /// Toggle thinking and secondary structured fields together.
    pub fn toggle(&mut self) {
        let visible = !self.thinking_visible;
        self.thinking_visible = visible;
        self.secondary_fields_visible = visible;
    }

    /// Human-readable status shown after the visibility keystroke.
    #[must_use]
    pub fn status_text(&self) -> String {
        let thinking = if self.thinking_visible { "on" } else { "off" };
        let details = if self.secondary_fields_visible {
            "on"
        } else {
            "off"
        };
        format!("thinking: {thinking}, details: {details}")
    }
}
