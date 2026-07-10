//! Cross-cutting utility modules.
//!
//! - [`frontmatter`] — shared `split_frontmatter` plus
//!   [`FrontmatterError`]. Used by the
//!   profile loader, the rules parser, and future skills.

pub mod frontmatter;
mod secure_file;

pub use frontmatter::{FrontmatterError, split_frontmatter};
pub(crate) use secure_file::{
    WorkspaceEntryKind, read_workspace_directory, read_workspace_text_file,
    validate_workspace_regular_file, workspace_file_mtime, workspace_relative_path,
};
