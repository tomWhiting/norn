//! Per-tool renderers for the rich (tier-1) tier.
//!
//! Each renderer lives in its own module so the file stays well under
//! the workspace's 500-line production-code cap (CO3) and so the AST
//! diff / syntax highlighting enhancements queued behind this can land
//! without forcing the next renderer to be split out under pressure.
//!
//! The module set is closed — adding a tier-1 tool means adding a new
//! file here, a `pub mod`/`pub use` pair below, and a match arm in
//! [`super::renderer::renderer_for`].

pub mod bash;
pub mod edit;
pub mod patch;
pub mod read;
pub mod search;

pub use bash::BashRenderer;
pub use edit::EditRenderer;
pub use patch::ApplyPatchRenderer;
pub use read::ReadRenderer;
pub use search::SearchRenderer;
