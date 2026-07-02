//! Shared filesystem-walk and glob helpers for the search sub-modules.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::ToolError;
use crate::tool::failure::ToolErrorKind;

/// A compiled glob filter that may contain multiple alternatives (from
/// brace expansion like `**/*.{rs,md,toml}`).
pub(super) struct GlobFilter {
    patterns: Vec<glob::Pattern>,
    filename_only: bool,
}

impl GlobFilter {
    pub(super) fn matches(&self, path: &Path) -> bool {
        let opts = glob_match_options();
        let check_path = if self.filename_only {
            path.file_name().map_or(path, Path::new)
        } else {
            path
        };
        self.patterns
            .iter()
            .any(|pat| pat.matches_path_with(check_path, opts))
    }
}

/// Compile an optional glob filter string into a [`GlobFilter`].
///
/// Supports brace expansion: `**/*.{rs,md}` expands to two patterns
/// (`**/*.rs` and `**/*.md`). Models frequently use this syntax.
/// When the pattern has no path separator, matching is done against
/// the filename component only (so `README.md` matches `./src/README.md`).
pub(super) fn compile_glob(glob_filter: Option<&str>) -> Result<Option<GlobFilter>, ToolError> {
    let Some(raw) = glob_filter else {
        return Ok(None);
    };
    let expanded = expand_braces(raw);
    let mut patterns = Vec::with_capacity(expanded.len());
    for pat_str in &expanded {
        patterns.push(glob::Pattern::new(pat_str).map_err(|e| {
            ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                format!("invalid glob `{pat_str}`: {e}"),
            )
        })?);
    }
    let filename_only = !raw.contains('/') && !raw.contains('\\') && !raw.starts_with("**");
    Ok(Some(GlobFilter {
        patterns,
        filename_only,
    }))
}

/// Expand brace alternation in a glob pattern.
///
/// `**/*.{rs,md,toml}` → `["**/*.rs", "**/*.md", "**/*.toml"]`
///
/// Handles multiple brace groups via recursion:
/// `{a,b}.{c,d}` → `["a.c", "a.d", "b.c", "b.d"]`
///
/// Does NOT handle nested braces (`{a,{b,c}}`). The first `}` always
/// closes the first `{`. Nested braces are not observed in model usage;
/// if they appear, this would need balanced-brace parsing.
///
/// If there are no braces, returns the input unchanged.
pub(super) fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let Some(close) = pattern[open..].find('}') else {
        return vec![pattern.to_owned()];
    };
    let close = open + close;
    let prefix = &pattern[..open];
    let suffix = &pattern[close + 1..];
    let alternatives = &pattern[open + 1..close];

    alternatives
        .split(',')
        .flat_map(|alt| expand_braces(&format!("{prefix}{alt}{suffix}")))
        .collect()
}

/// Match options for glob patterns.
pub(super) fn glob_match_options() -> glob::MatchOptions {
    glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    }
}

/// A filesystem entry the walk could not process, reported to the model so
/// an empty result set is never mistaken for a verified "no matches".
#[derive(Debug, Serialize)]
pub(super) struct SkippedEntry {
    /// Path of the entry (or its closest known ancestor) that was skipped.
    pub(super) path: String,
    /// Human-readable reason the entry was skipped.
    pub(super) reason: String,
}

/// One entry discovered by [`walk_tree`].
pub(super) struct WalkedEntry {
    /// Absolute (root-relative-joined) path of the entry.
    pub(super) path: PathBuf,
    /// Whether the entry is a regular file (symlinks are not followed).
    pub(super) is_file: bool,
}

/// The outcome of walking a search root.
pub(super) struct WalkedTree {
    /// All entries below the root (files, directories, symlinks), sorted by
    /// path for deterministic output. The root itself is not included.
    pub(super) entries: Vec<WalkedEntry>,
    /// Entries the walk failed to traverse, with reasons.
    pub(super) skipped: Vec<SkippedEntry>,
}

/// Walk `root`, honouring gitignore/hidden-file rules unless
/// `include_ignored` is set.
///
/// Ignore rules are applied even outside git repositories
/// (`require_git(false)`) so a `.gitignore` in a plain directory tree
/// behaves identically to one in a checked-out repository. Traversal
/// failures (unreadable directories, unresolvable entries) are collected
/// into `skipped` rather than silently dropping subtrees.
///
/// When `root` is a regular file (not a directory) it is returned as the
/// single entry, bypassing ignore rules — an explicitly named file is
/// always searched.
pub(super) fn walk_tree(root: &Path, include_ignored: bool) -> WalkedTree {
    let mut entries: Vec<WalkedEntry> = Vec::new();
    let mut skipped: Vec<SkippedEntry> = Vec::new();

    if root.is_file() {
        entries.push(WalkedEntry {
            path: root.to_path_buf(),
            is_file: true,
        });
        return WalkedTree { entries, skipped };
    }

    let mut builder = ignore::WalkBuilder::new(root);
    if include_ignored {
        builder.standard_filters(false);
    } else {
        builder.require_git(false);
    }

    for result in builder.build() {
        match result {
            Ok(entry) => {
                if entry.depth() == 0 {
                    continue;
                }
                match entry.file_type() {
                    Some(file_type) => {
                        let is_file = file_type.is_file();
                        entries.push(WalkedEntry {
                            path: entry.into_path(),
                            is_file,
                        });
                    }
                    None => skipped.push(SkippedEntry {
                        path: entry.path().to_string_lossy().into_owned(),
                        reason: "entry has no resolvable file type".to_owned(),
                    }),
                }
            }
            Err(e) => skipped.push(skipped_from_walk_error(&e, root)),
        }
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    WalkedTree { entries, skipped }
}

/// Convert a walker error into a [`SkippedEntry`], attributing it to the
/// most specific path the error carries (falling back to the walk root).
fn skipped_from_walk_error(err: &ignore::Error, root: &Path) -> SkippedEntry {
    let path = walk_error_path(err)
        .unwrap_or(root)
        .to_string_lossy()
        .into_owned();
    SkippedEntry {
        path,
        reason: err.to_string(),
    }
}

/// Extract the most specific path carried by a walker error, if any.
fn walk_error_path(err: &ignore::Error) -> Option<&Path> {
    match err {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithLineNumber { err, .. } | ignore::Error::WithDepth { err, .. } => {
            walk_error_path(err)
        }
        ignore::Error::Partial(errs) => errs.iter().find_map(walk_error_path),
        ignore::Error::Loop { child, .. } => Some(child),
        ignore::Error::Io(_)
        | ignore::Error::Glob { .. }
        | ignore::Error::UnrecognizedFileType(_)
        | ignore::Error::InvalidDefinition => None,
    }
}
