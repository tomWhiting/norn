//! Cross-cutting utility modules.
//!
//! - [`frontmatter`] — shared `split_frontmatter` plus
//!   [`FrontmatterError`]. Used by the
//!   profile loader, the rules parser, and future skills.

pub mod frontmatter;
mod private_file_identity;
mod private_fs;
mod process_signal;
mod secure_file;

pub use frontmatter::{FrontmatterError, split_frontmatter};
pub(crate) use private_file_identity::PrivateFileIdentity;
pub use private_fs::validate_private_component;
pub(crate) use private_fs::{
    PrivateDirEntry, PrivateEntryKind, PrivateRoot, PrivateRootReader, PrivateTreeEntry,
};
#[cfg(unix)]
pub(crate) use process_signal::kill_process_group;
pub(crate) use secure_file::{
    WorkspaceEntryKind, read_workspace_directory, read_workspace_text_file,
    validate_workspace_regular_file, workspace_file_mtime, workspace_relative_path,
};
