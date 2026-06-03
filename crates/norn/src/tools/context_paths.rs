//! Context-discovery search-path extension.
//!
//! Mirrors [`crate::tools::skill::SkillSearchPaths`] in shape: a newtype
//! over `Vec<PathBuf>` that the CLI installs on the shared
//! [`crate::tool::context::ToolContext`] when `settings.context.search_paths`
//! is populated. Harry's context cluster (NG2) consumes it via
//! `ctx.get_extension::<ContextSearchPaths>()` to discover context
//! fragments such as `CLAUDE.md` or `AGENTS.md`.
//!
//! There are no compiled-in defaults — absence of the extension means the
//! operator did not configure any settings-level search paths, and the
//! consuming cluster is free to apply its own discovery rules.

use std::path::PathBuf;

/// Directories scanned for context fragments, in order.
///
/// Populated from `settings.context.search_paths` in `NornSettings`.
/// Entries are absolute paths produced by joining each settings value
/// (when relative) onto the working directory at runtime-assembly time.
pub struct ContextSearchPaths(pub Vec<PathBuf>);
