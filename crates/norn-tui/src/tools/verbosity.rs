//! Tool-call render verbosity toggle.

/// Controls whether tool calls render in collapsed (header only) or
/// expanded (header + body) mode. Toggled by Ctrl+O.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VerbosityState {
    /// Show header only. Body content is available but not rendered to
    /// the scroll region. Ctrl+O toggles to Expanded.
    Collapsed,
    /// Show header followed by body (when `body()` returns `Some`).
    #[default]
    Expanded,
}

impl VerbosityState {
    /// Flip between [`Collapsed`](Self::Collapsed) and
    /// [`Expanded`](Self::Expanded).
    #[must_use]
    pub fn toggle(self) -> Self {
        match self {
            Self::Collapsed => Self::Expanded,
            Self::Expanded => Self::Collapsed,
        }
    }

    /// Human-readable label for the streaming indicator flash.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Collapsed => "verbose: off",
            Self::Expanded => "verbose: on",
        }
    }
}
