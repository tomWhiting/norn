//! Error types for the norn-tui crate.

use std::io;

/// Errors that can occur during TUI operation.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    /// Direct Markdown styling or original/display mapping failed.
    #[error(transparent)]
    Markdown(#[from] crate::render::retained_markdown::MarkdownError),
    /// Display text or grapheme geometry was invalid.
    #[error(transparent)]
    DisplayText(#[from] crate::render::retained_text::TextError),
    /// The requested split could not be represented.
    #[error(transparent)]
    Layout(#[from] crate::render::layout::LayoutError),
    /// A prepared terminal coordinate could not be represented.
    #[error("terminal coordinate {value} cannot be represented: {source}")]
    FrameCoordinate {
        /// Actual unrepresentable coordinate or row index.
        value: usize,
        /// Checked integer conversion failure.
        source: std::num::TryFromIntError,
    },
    /// Formatting recorded tool evidence failed before any frame was published.
    #[error("formatting Changes for item {item:?} failed: {source}")]
    ChangeFormatting {
        /// Exact selected call item.
        item: Box<norn::session_view::ItemId>,
        /// Formatter failure retained as its source.
        source: std::fmt::Error,
    },
    /// A prepared frame exceeded its declared rectangle.
    #[error("prepared frame exceeds its declared terminal rectangle")]
    FrameBounds,
    /// A local view action could not preserve its source or body identity.
    #[error("view interaction failed: {source}")]
    ViewInteraction {
        /// Typed internal focus or viewport validation error.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The terminal does not meet minimum requirements for the TUI.
    #[error("unsupported terminal: {0}")]
    UnsupportedTerminal(String),

    /// An I/O error occurred during terminal operations.
    #[error(transparent)]
    Io(#[from] io::Error),

    /// The installed channel owner could not provide idle wake readiness.
    #[error(transparent)]
    McpChannel(#[from] norn::integration::McpChannelError),
    /// Shared semantic source, cursor or body validation failed.
    #[error(transparent)]
    View(#[from] norn::session_view::ViewError),

    /// The actual store owner refused a source-bound history/body read.
    #[error(transparent)]
    ViewRead(#[from] norn::session::store::HistoryReadError),

    /// A declared frontend demand was invalid.
    #[error("view {name} demand must be positive; received {value}")]
    InvalidViewDemand {
        /// Named preference.
        name: &'static str,
        /// Rejected demand.
        value: usize,
    },

    /// An explicit background read failed to complete.
    #[error("view {operation} task failed: {source}")]
    ViewTask {
        /// Read operation being performed.
        operation: &'static str,
        /// Join/cancellation/panic evidence from its task owner.
        source: tokio::task::JoinError,
    },

    /// A body result did not match the requested original-byte range.
    #[error("view body page for {item:?} is not contiguous at byte {offset}")]
    InvalidBodyPage {
        /// Exact requested semantic item.
        item: Box<norn::session_view::ItemId>,
        /// Rejected original-byte position.
        offset: usize,
    },
}
