//! Cross-cutting utility modules.
//!
//! - [`frontmatter`] — shared `split_frontmatter` plus
//!   [`FrontmatterError`](frontmatter::FrontmatterError). Used by the
//!   profile loader, the rules parser, and future skills.

pub mod frontmatter;

pub use frontmatter::{FrontmatterError, split_frontmatter};
