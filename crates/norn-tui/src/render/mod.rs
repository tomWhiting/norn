//! Rendering primitives — style system, scroll region helpers, fixed panel compositor.

pub(crate) mod composer_cells;
pub mod content;
pub mod fixed_panel;
pub mod frame;
pub mod layout;
pub mod markdown;
pub mod retained_markdown;
pub mod retained_structured;
pub mod retained_text;
pub mod retry_status;
pub mod streaming_indicator;
pub mod style;
pub mod syntax;
pub mod text;
pub mod thinking;

pub use fixed_panel::{FixedPanel, StatusBar};
pub use markdown::MarkdownRenderer;
pub use retry_status::{RETRY_ACTIVITY_PREFIX, retry_status_label, retry_wait_secs};
pub use streaming_indicator::{StreamingIndicator, ToolUseInFlight};
pub use style::{
    colour_for, colour_spec, hyperlink, italic, italic_off, nearest_256, newline_key_hint,
    sync_render,
};
pub use syntax::SyntaxHighlighter;
