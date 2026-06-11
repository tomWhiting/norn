//! Shared filesystem-walk and glob helpers for the search sub-modules.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::error::ToolError;
use crate::tool::failure::ToolErrorKind;

use super::SearchMatch;

/// A compiled glob filter that may contain multiple alternatives (from
/// brace expansion like `**/*.{rs,md,toml}`).
pub(super) struct GlobFilter {
    patterns: Vec<glob::Pattern>,
    filename_only: bool,
}

impl GlobFilter {
    fn matches(&self, path: &Path) -> bool {
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
fn glob_match_options() -> glob::MatchOptions {
    glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    }
}

/// Search `root` for regex matches in file content.
///
/// When `root` is a regular file (not a directory), searches that single
/// file directly. When `root` is a directory, walks it recursively.
pub(super) fn walk_for_content(
    root: &Path,
    regex: &Regex,
    glob_filter: Option<&GlobFilter>,
    max_results: u32,
    out: &mut Vec<SearchMatch>,
    truncated: &mut bool,
) {
    if root.is_file() {
        search_single_file(root, regex, glob_filter, max_results, out, truncated);
        return;
    }
    walk_dir_for_content(root, regex, glob_filter, max_results, out, truncated);
}

fn search_single_file(
    path: &Path,
    regex: &Regex,
    glob_filter: Option<&GlobFilter>,
    max_results: u32,
    out: &mut Vec<SearchMatch>,
    truncated: &mut bool,
) {
    if let Some(filter) = glob_filter
        && !filter.matches(path)
    {
        return;
    }
    // Binary / non-UTF-8 / unreadable files are skipped — content search
    // is best-effort across heterogeneous trees.
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    let cap = max_results as usize;
    for (idx, line) in contents.lines().enumerate() {
        if regex.is_match(line) {
            if out.len() >= cap {
                *truncated = true;
                return;
            }
            let line_number = u32::try_from(idx + 1).unwrap_or(u32::MAX);
            out.push(SearchMatch {
                path: path.to_string_lossy().into_owned(),
                line: line_number,
                content: line.to_owned(),
            });
        }
    }
}

fn walk_dir_for_content(
    dir: &Path,
    regex: &Regex,
    glob_filter: Option<&GlobFilter>,
    max_results: u32,
    out: &mut Vec<SearchMatch>,
    truncated: &mut bool,
) {
    if *truncated {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let cap = max_results as usize;

    for entry in entries.flatten() {
        if *truncated {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            walk_dir_for_content(&path, regex, glob_filter, max_results, out, truncated);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        if let Some(filter) = glob_filter
            && !filter.matches(&path)
        {
            continue;
        }

        // Binary / non-UTF-8 / unreadable files skipped — best-effort.
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };

        for (idx, line) in contents.lines().enumerate() {
            if regex.is_match(line) {
                if out.len() >= cap {
                    *truncated = true;
                    return;
                }
                let line_number = u32::try_from(idx + 1).unwrap_or(u32::MAX);
                out.push(SearchMatch {
                    path: path.to_string_lossy().into_owned(),
                    line: line_number,
                    content: line.to_owned(),
                });
            }
        }
    }
}

/// Collect file paths under `root`, filtered by an optional glob.
///
/// When `root` is a regular file (not a directory), returns that single
/// file if it passes the filter.
pub(super) fn walk_collect_paths(
    root: &Path,
    glob_filter: Option<&GlobFilter>,
    out: &mut Vec<PathBuf>,
) {
    if root.is_file() {
        if let Some(filter) = glob_filter
            && !filter.matches(root)
        {
            return;
        }
        out.push(root.to_path_buf());
        return;
    }
    walk_dir_collect_paths(root, glob_filter, out);
}

fn walk_dir_collect_paths(dir: &Path, glob_filter: Option<&GlobFilter>, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            walk_dir_collect_paths(&path, glob_filter, out);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if let Some(filter) = glob_filter
            && !filter.matches(&path)
        {
            continue;
        }
        out.push(path);
    }
}
