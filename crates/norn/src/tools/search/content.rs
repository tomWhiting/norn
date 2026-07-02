//! Regex content search over a walked file tree.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use regex::Regex;
use serde::Serialize;
use serde_json::json;

use crate::error::ToolError;
use crate::tool::failure::ToolErrorKind;
use crate::tool::traits::ToolOutput;

use super::helpers::{GlobFilter, SkippedEntry, compile_glob, walk_tree};

/// A single content-search hit.
#[derive(Debug, Serialize)]
pub(super) struct SearchMatch {
    path: String,
    line: u32,
    content: String,
}

/// Run a regex content search under `root`, returning matching lines as
/// `{path, line, content}` tuples plus skipped-entry metadata.
pub(super) fn run_content_search(
    pattern: &str,
    root: &Path,
    glob_filter: Option<&str>,
    max_results: u32,
    include_ignored: bool,
) -> Result<ToolOutput, ToolError> {
    let regex = Regex::new(pattern).map_err(|e| {
        ToolError::pre_validation(
            ToolErrorKind::InvalidArguments,
            format!("invalid regex `{pattern}`: {e}"),
        )
    })?;

    let compiled_filter: Option<GlobFilter> = compile_glob(glob_filter)?;

    let walked = walk_tree(root, include_ignored);
    let mut skipped = walked.skipped;
    let mut matches: Vec<SearchMatch> = Vec::new();
    let mut truncated = false;
    let cap = max_results as usize;

    'files: for entry in walked.entries.iter().filter(|e| e.is_file) {
        if let Some(filter) = compiled_filter.as_ref()
            && !filter.matches(&entry.path)
        {
            continue;
        }

        let contents = match fs::read_to_string(&entry.path) {
            Ok(contents) => contents,
            // Binary / non-UTF-8 content carries no text lines for the
            // regex to match, so it is not a lost result.
            Err(e) if e.kind() == ErrorKind::InvalidData => continue,
            Err(e) => {
                skipped.push(SkippedEntry {
                    path: entry.path.to_string_lossy().into_owned(),
                    reason: format!("unreadable: {e}"),
                });
                continue;
            }
        };

        for (idx, line) in contents.lines().enumerate() {
            if regex.is_match(line) {
                if matches.len() >= cap {
                    truncated = true;
                    break 'files;
                }
                let line_number = u32::try_from(idx + 1).unwrap_or(u32::MAX);
                matches.push(SearchMatch {
                    path: entry.path.to_string_lossy().into_owned(),
                    line: line_number,
                    content: line.to_owned(),
                });
            }
        }
    }

    Ok(ToolOutput::success(json!({
        "matches": matches,
        "truncated": truncated,
        "skipped": skipped,
    })))
}
