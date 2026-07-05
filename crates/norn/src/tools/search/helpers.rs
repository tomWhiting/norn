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
/// `.git` entries encountered during the walk are never descended into,
/// even with `include_ignored` set: the `ignore` crate only excludes
/// `.git` as a by-product of its hidden-file filter, so disabling the
/// standard filters would otherwise pour VCS internals (objects, packs,
/// hooks) into results meant for dotfiles and gitignored artefacts.
/// Naming a `.git` directory as the walk root still works — the
/// exclusion applies below the root only, so a deliberate look inside
/// git plumbing remains possible.
///
/// Files matching [`is_sensitive_file_name`] (environment files, private
/// keys, certificates, credential files) are excluded from every walk —
/// default and `include_ignored` alike — and reported in `skipped` so an
/// empty result is never mistaken for "no such file". Owner ruling (Tom,
/// 2026-07-06): search must never sweep secret material into session
/// logs incidentally; deliberate access goes through `read` or an
/// explicitly named path.
///
/// When `root` is a regular file (not a directory) it is returned as the
/// single entry, bypassing ignore rules and the sensitive-file exclusion
/// — an explicitly named file is always searched.
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
    builder.filter_entry(|entry| {
        entry.depth() == 0 || entry.file_name() != std::ffi::OsStr::new(".git")
    });

    for result in builder.build() {
        match result {
            Ok(entry) => {
                if entry.depth() == 0 {
                    continue;
                }
                match entry.file_type() {
                    Some(file_type) => {
                        let is_file = file_type.is_file();
                        if is_file && is_sensitive_file_name(entry.file_name()) {
                            skipped.push(SkippedEntry {
                                path: entry.path().to_string_lossy().into_owned(),
                                reason: SENSITIVE_SKIP_REASON.to_owned(),
                            });
                            continue;
                        }
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

/// Reason string attached to sensitive-file exclusions, telling the model
/// how to proceed deliberately instead of concluding "no matches".
const SENSITIVE_SKIP_REASON: &str = "excluded from search: likely secret material \
     (environment/key/certificate/credential file); use the read tool on the exact \
     path if you genuinely need its contents";

/// File extensions that denote environment, key, or certificate material.
///
/// Compared case-insensitively against the final `.`-separated component,
/// so `env` here covers both `.env` itself and `production.env`.
const SENSITIVE_EXTENSIONS: &[&str] = &[
    "env", "pem", "key", "p12", "pfx", "der", "ppk", "jks", "crt", "cer",
];

/// Exact filenames that are credential stores by definition.
const SENSITIVE_FILENAMES: &[&str] = &[
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    ".netrc",
    ".pgpass",
    ".htpasswd",
];

/// Whether a filename denotes always-sensitive material that search walks
/// must never surface incidentally (owner ruling, Tom 2026-07-06).
///
/// Covers environment files in their common shapes (`.env`, `.env.local`,
/// `production.env`), key/certificate material by extension, and canonical
/// credential filenames (SSH private keys, `.netrc`, `.pgpass`,
/// `.htpasswd`). Matching is case-insensitive on the filename only —
/// non-UTF-8 names cannot match any pattern and are treated as
/// non-sensitive.
pub(super) fn is_sensitive_file_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    // `.env.local` and friends: the extension check below only sees the
    // final component (`local`), so the `.env.` prefix is matched here.
    if lower.starts_with(".env.") {
        return true;
    }
    if let Some((_, ext)) = lower.rsplit_once('.')
        && SENSITIVE_EXTENSIONS.contains(&ext)
    {
        return true;
    }
    SENSITIVE_FILENAMES.contains(&lower.as_str())
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

#[cfg(test)]
mod sensitive_name_tests {
    use super::is_sensitive_file_name;
    use std::ffi::OsStr;

    #[test]
    fn env_files_in_all_common_shapes_are_sensitive() {
        for name in [
            ".env",
            ".env.local",
            ".env.production",
            "production.env",
            ".ENV",
        ] {
            assert!(is_sensitive_file_name(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn key_and_certificate_extensions_are_sensitive() {
        for name in [
            "server.pem",
            "SERVER.PEM",
            "private.key",
            ".key",
            "bundle.p12",
            "store.pfx",
            "cert.der",
            "login.ppk",
            "trust.jks",
            "site.crt",
            "site.cer",
        ] {
            assert!(is_sensitive_file_name(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn credential_filenames_are_sensitive() {
        for name in [
            "id_rsa",
            "id_dsa",
            "id_ecdsa",
            "id_ed25519",
            ".netrc",
            ".pgpass",
            ".htpasswd",
        ] {
            assert!(is_sensitive_file_name(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn ordinary_and_public_files_are_not_sensitive() {
        for name in [
            "envelope.txt",
            "environment.rs",
            "monkey.txt",
            "README.md",
            "id_rsa.pub",
            "keyboard.rs",
            "env",
            "Cargo.toml",
        ] {
            assert!(!is_sensitive_file_name(OsStr::new(name)), "{name}");
        }
    }
}
