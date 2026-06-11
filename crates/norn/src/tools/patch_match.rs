//! Context-window matching primitives for patch application.
//!
//! Shared by the unified-diff resolver (`patch_apply`) and the Claude Code
//! applicator (`patch_cc`). Matching tries three strategies in order —
//! exact byte equality, whitespace-insensitive (runs of whitespace
//! collapsed), then trim-insensitive — and, unlike a naive first-match
//! scan, is **occurrence-aware**: every window matching the winning
//! strategy is collected, the hunk's stated line is used to prefer the
//! nearest candidate, and when nothing disambiguates equally-good
//! candidates the caller receives an [`AmbiguousMatch`] so it can refuse
//! instead of silently patching the wrong occurrence.

/// Which matching strategy located a context window. Drives the confidence
/// recorded for a tier-2 resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MatchKind {
    /// Byte-identical match.
    Exact,
    /// Match after collapsing every run of whitespace to a single space.
    Whitespace,
    /// Match after trimming each line.
    Trim,
}

/// Multiple context windows matched equally well and no line information
/// disambiguates them. Carries the 1-based line numbers of every candidate
/// so the caller can report them and refuse.
#[derive(Clone, Debug)]
pub(super) struct AmbiguousMatch {
    /// 1-based line numbers where the pattern matched.
    pub(super) candidate_lines: Vec<usize>,
}

impl AmbiguousMatch {
    /// Renders the candidate list for error messages, e.g. `lines 3, 17, 41`.
    pub(super) fn describe_candidates(&self) -> String {
        let joined = self
            .candidate_lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("lines {joined}")
    }
}

/// Collapse every run of whitespace in `s` to a single space, so context lines
/// that differ only in whitespace *quantity* (e.g. tabs vs spaces, double vs
/// single space) compare equal.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(ch);
            in_ws = false;
        }
    }
    out
}

/// Returns every 0-based window start where `pattern_lines` matches
/// `file_lines` under `eq`.
fn positions_matching(
    file_lines: &[&str],
    pattern_lines: &[&str],
    eq: impl Fn(&str, &str) -> bool,
) -> Vec<usize> {
    if pattern_lines.is_empty() || pattern_lines.len() > file_lines.len() {
        return Vec::new();
    }
    let window_len = pattern_lines.len();
    (0..=file_lines.len() - window_len)
        .filter(|&i| {
            file_lines[i..i + window_len]
                .iter()
                .zip(pattern_lines.iter())
                .all(|(a, b)| eq(a, b))
        })
        .collect()
}

/// Finds every match of `pattern_lines` in `file_lines` for the *strongest*
/// strategy that matches at all: exact first, then whitespace-insensitive,
/// then trim-insensitive. Returns the 0-based positions plus the strategy.
pub(super) fn find_all_context_matches(
    file_lines: &[&str],
    pattern_lines: &[&str],
) -> Option<(Vec<usize>, MatchKind)> {
    let exact = positions_matching(file_lines, pattern_lines, |a, b| a == b);
    if !exact.is_empty() {
        return Some((exact, MatchKind::Exact));
    }
    let ws = positions_matching(file_lines, pattern_lines, |a, b| {
        collapse_ws(a) == collapse_ws(b)
    });
    if !ws.is_empty() {
        return Some((ws, MatchKind::Whitespace));
    }
    let trim = positions_matching(file_lines, pattern_lines, |a, b| a.trim() == b.trim());
    if !trim.is_empty() {
        return Some((trim, MatchKind::Trim));
    }
    None
}

/// Occurrence-aware context search.
///
/// Finds all matches via [`find_all_context_matches`] and selects one:
///
/// * no match → `Ok(None)`;
/// * exactly one match → that match (a unique context is its own
///   disambiguation, regardless of drift from the stated line);
/// * several matches → the candidate nearest to `expected_pos` (the 0-based
///   position the hunk header claims, adjusted for already-applied hunks)
///   when that nearest candidate is unique;
/// * several matches tied for nearest (or no usable expected position) →
///   `Err(AmbiguousMatch)` so the caller refuses instead of guessing.
pub(super) fn select_context_match(
    file_lines: &[&str],
    pattern_lines: &[&str],
    expected_pos: Option<usize>,
) -> Result<Option<(usize, MatchKind)>, AmbiguousMatch> {
    let Some((positions, kind)) = find_all_context_matches(file_lines, pattern_lines) else {
        return Ok(None);
    };
    match select_position(&positions, expected_pos) {
        Some(pos) => Ok(Some((pos, kind))),
        None => Err(AmbiguousMatch {
            candidate_lines: positions.iter().map(|p| p + 1).collect(),
        }),
    }
}

/// Same as [`select_context_match`] but restricted to
/// `[range_start, range_end)` of `file_lines`; returned positions are
/// relative to the full file.
pub(super) fn select_context_match_in_range(
    file_lines: &[&str],
    pattern_lines: &[&str],
    range_start: usize,
    range_end: usize,
    expected_pos: Option<usize>,
) -> Result<Option<(usize, MatchKind)>, AmbiguousMatch> {
    if pattern_lines.is_empty() || range_end <= range_start {
        return Ok(None);
    }
    let end = range_end.min(file_lines.len());
    let start = range_start.min(end);
    let relative_expected = expected_pos.map(|e| e.saturating_sub(start));
    match select_context_match(&file_lines[start..end], pattern_lines, relative_expected) {
        Ok(found) => Ok(found.map(|(pos, kind)| (pos + start, kind))),
        Err(ambiguous) => Err(AmbiguousMatch {
            candidate_lines: ambiguous
                .candidate_lines
                .iter()
                .map(|line| line + start)
                .collect(),
        }),
    }
}

/// Picks the single candidate, or the unique nearest candidate to
/// `expected_pos`. Returns `None` when the candidates cannot be
/// disambiguated (tied distances, or several candidates with no expected
/// position).
fn select_position(positions: &[usize], expected_pos: Option<usize>) -> Option<usize> {
    match positions {
        [] => None,
        [only] => Some(*only),
        many => {
            let expected = expected_pos?;
            let distance = |p: usize| p.abs_diff(expected);
            let best = many.iter().copied().min_by_key(|&p| distance(p))?;
            let tied = many.iter().filter(|&&p| distance(p) == distance(best));
            (tied.count() == 1).then_some(best)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_found() {
        let file: Vec<&str> = vec!["a", "b", "c", "d"];
        assert_eq!(
            select_context_match(&file, &["b", "c"], None).unwrap(),
            Some((1, MatchKind::Exact))
        );
    }

    #[test]
    fn whitespace_collapse_match() {
        let file: Vec<&str> = vec!["fn  main()", "let  x = 1", "c"];
        assert_eq!(
            select_context_match(&file, &["fn main()", "let x = 1"], None).unwrap(),
            Some((0, MatchKind::Whitespace))
        );
    }

    #[test]
    fn trimmed_match() {
        let file: Vec<&str> = vec!["a  ", "b  ", "c", "d"];
        assert_eq!(
            select_context_match(&file, &["a", "b"], None).unwrap(),
            Some((0, MatchKind::Trim))
        );
    }

    #[test]
    fn no_match_is_ok_none() {
        let file: Vec<&str> = vec!["a", "b", "c"];
        assert_eq!(
            select_context_match(&file, &["x", "y"], None).unwrap(),
            None
        );
    }

    #[test]
    fn unique_match_applies_despite_large_drift() {
        let file: Vec<&str> = vec!["pad"; 50]
            .into_iter()
            .chain(["target", "body"])
            .collect();
        // Expected position 0, actual at 50 — unique context wins anyway.
        assert_eq!(
            select_context_match(&file, &["target", "body"], Some(0)).unwrap(),
            Some((50, MatchKind::Exact))
        );
    }

    #[test]
    fn duplicate_matches_prefer_nearest_to_stated_line() {
        let file: Vec<&str> = vec!["fn a() {", "    x();", "}", "", "fn b() {", "    x();", "}"];
        let pattern = ["    x();"];
        // Stated position points at the second occurrence (index 5).
        assert_eq!(
            select_context_match(&file, &pattern, Some(5)).unwrap(),
            Some((5, MatchKind::Exact))
        );
        // And at the first occurrence (index 1).
        assert_eq!(
            select_context_match(&file, &pattern, Some(1)).unwrap(),
            Some((1, MatchKind::Exact))
        );
    }

    #[test]
    fn equidistant_duplicates_are_ambiguous() {
        let file: Vec<&str> = vec!["dup", "mid", "dup"];
        let err = select_context_match(&file, &["dup"], Some(1)).unwrap_err();
        assert_eq!(err.candidate_lines, vec![1, 3]);
        assert_eq!(err.describe_candidates(), "lines 1, 3");
    }

    #[test]
    fn duplicates_without_expected_position_are_ambiguous() {
        let file: Vec<&str> = vec!["dup", "x", "dup"];
        let err = select_context_match(&file, &["dup"], None).unwrap_err();
        assert_eq!(err.candidate_lines, vec![1, 3]);
    }

    #[test]
    fn range_restricted_search_offsets_results() {
        let file: Vec<&str> = vec!["x", "y", "x", "y"];
        let found = select_context_match_in_range(&file, &["x", "y"], 2, 4, None).unwrap();
        assert_eq!(found, Some((2, MatchKind::Exact)));
    }

    #[test]
    fn range_restricted_ambiguity_reports_absolute_lines() {
        let file: Vec<&str> = vec!["pad", "dup", "dup", "dup"];
        let err = select_context_match_in_range(&file, &["dup"], 1, 4, None).unwrap_err();
        assert_eq!(err.candidate_lines, vec![2, 3, 4]);
    }

    #[test]
    fn exact_strategy_shadows_weaker_duplicates() {
        // One exact match plus a trim-level near-duplicate: the exact
        // strategy wins outright, so no ambiguity arises.
        let file: Vec<&str> = vec!["line ", "line"];
        assert_eq!(
            select_context_match(&file, &["line"], None).unwrap(),
            Some((1, MatchKind::Exact))
        );
    }
}
