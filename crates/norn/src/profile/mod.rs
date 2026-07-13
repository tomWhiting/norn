//! Agent profile, capability, and prompt-command configuration.
//!
//! - [`types`] — [`Profile`], [`Capability`], [`PromptCommand`] and
//!   [`Profile::from_file`] (the extension-dispatched loader).
//! - [`loader`] — `parse_profile` / `parse_capability` plus scanner helpers.
//!   Delegates to [`crate::util::frontmatter::split_frontmatter`] for the
//!   YAML-frontmatter split.
//! - [`resolve`] — capability resolution helpers on [`Profile`] and the
//!   [`from_profile`] [`crate::agent_loop::loop_context::LoopContext`] builder.

pub mod loader;
pub mod resolve;
mod scanner;
pub mod types;

pub use loader::{
    ProfileOrigin, ResolvedWorkspaceProfile, capability_scan_dirs, default_scan_dirs,
    parse_capability, parse_profile, resolve_capability, resolve_profile,
    resolve_profile_capabilities, resolve_workspace_profile,
};
pub use resolve::from_profile;
pub use scanner::Scanner;
pub use types::{Capability, Profile, PromptCommand};
