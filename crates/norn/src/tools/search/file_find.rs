//! Glob-based file finding over a walked tree.

use std::path::Path;

use serde_json::json;

use crate::error::ToolError;
use crate::tool::failure::ToolErrorKind;
use crate::tool::traits::ToolOutput;

use super::helpers::{SkippedEntry, expand_braces, walk_tree};

/// Expand `glob_pattern` against the filesystem under `base` and return
/// matching paths (files and directories) plus skipped-entry metadata.
///
/// The pattern is matched against each entry's path *relative to* `base`,
/// never against a string that embeds `base` itself — so glob
/// metacharacters (`[`, `]`, `*`, `?`) in the base directory's own name
/// are treated literally.
pub(super) fn run_file_find(
    base: &Path,
    glob_pattern: &str,
    max_results: u32,
    include_ignored: bool,
) -> Result<ToolOutput, ToolError> {
    let expanded = expand_braces(glob_pattern);
    let mut patterns = Vec::with_capacity(expanded.len());
    for pat_str in &expanded {
        patterns.push(glob::Pattern::new(pat_str).map_err(|e| {
            ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                format!("invalid glob `{pat_str}`: {e}"),
            )
        })?);
    }

    let walked = walk_tree(base, include_ignored);
    let mut skipped = walked.skipped;
    // `*` and `?` must not cross `/`, matching how a filesystem glob
    // expands one path component at a time; `**` still spans components.
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };

    let mut paths: Vec<String> = Vec::new();
    for entry in &walked.entries {
        let relative = match entry.path.strip_prefix(base) {
            Ok(relative) => relative,
            Err(e) => {
                skipped.push(SkippedEntry {
                    path: entry.path.to_string_lossy().into_owned(),
                    reason: format!("entry is not under the search root: {e}"),
                });
                continue;
            }
        };
        if patterns
            .iter()
            .any(|pat| pat.matches_path_with(relative, opts))
        {
            paths.push(entry.path.to_string_lossy().into_owned());
        }
    }

    let cap = max_results as usize;
    let truncated = paths.len() > cap;
    if truncated {
        paths.truncate(cap);
    }

    Ok(ToolOutput::success(json!({
        "paths": paths,
        "truncated": truncated,
        "skipped": skipped,
    })))
}
