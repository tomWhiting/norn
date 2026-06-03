//! Fuzzy file-path matching via the `nucleo-matcher` algorithm.
//!
//! Scores file paths against a needle string and returns them ranked
//! best-first as `{path, score}` pairs.

use std::path::Path;
use std::time::Instant;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern as NucleoPattern};
use nucleo_matcher::{Config as NucleoConfig, Matcher as NucleoMatcher};
use serde::Serialize;
use serde_json::json;

use crate::error::ToolError;
use crate::tool::traits::ToolOutput;

use super::helpers::{GlobFilter, compile_glob, elapsed, walk_collect_paths};

/// A single fuzzy-match hit.
#[derive(Debug, Serialize)]
pub(super) struct FuzzyMatch {
    path: String,
    score: u32,
}

/// Run a fuzzy file-path search under `root`, scoring each candidate path
/// against `needle` using the nucleo matcher. Returns results ranked
/// best-first, capped at `max_results`.
pub(super) fn run_fuzzy_search(
    needle: &str,
    root: &Path,
    glob_filter: Option<&str>,
    max_results: u32,
    started: Instant,
) -> Result<ToolOutput, ToolError> {
    let compiled_filter: Option<GlobFilter> = compile_glob(glob_filter)?;

    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    walk_collect_paths(root, compiled_filter.as_ref(), &mut paths);
    let path_strings: Vec<String> = paths
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    let mut matcher = NucleoMatcher::new(NucleoConfig::DEFAULT.match_paths());
    let pat = NucleoPattern::parse(needle, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(String, u32)> = pat.match_list(path_strings, &mut matcher);

    let cap = max_results as usize;
    let truncated = scored.len() > cap;
    if truncated {
        scored.truncate(cap);
    }

    let results: Vec<FuzzyMatch> = scored
        .into_iter()
        .map(|(path, score)| FuzzyMatch { path, score })
        .collect();

    Ok(ToolOutput {
        content: json!({
            "matches": results,
            "truncated": truncated,
        }),
        is_error: false,
        duration: elapsed(started),
    })
}
