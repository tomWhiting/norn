//! Rendering primitives — style system, scroll region helpers, fixed panel compositor.

pub mod content;
pub mod fixed_panel;
pub mod markdown;
pub mod scroll_region;
pub mod style;
pub mod syntax;
pub mod text;

pub use fixed_panel::{FixedPanel, StatusBar, StreamingIndicator};
pub use markdown::MarkdownRenderer;
pub use scroll_region::{write_separator, write_to_scroll};
pub use style::{
    colour_for, colour_spec, hyperlink, italic, nearest_256, newline_key_hint, sync_render,
};
pub use syntax::SyntaxHighlighter;
