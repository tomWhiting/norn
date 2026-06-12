//! Hunk model and unified-diff hunk parsing for `apply_patch`.
//!
//! A single-file unified diff block is parsed into [`ParsedHunk`]s that
//! preserve the raw interleaving of context/remove/add lines, the 1-based
//! `old_start` from the `@@` header, the optional semantic anchor (the text
//! after the second `@@`), and the `\ No newline at end of file` markers for
//! both the old and new sides of the hunk.

/// A single line in a parsed hunk, preserving the interleaving order.
#[derive(Clone, Debug)]
pub(super) enum DiffLine {
    /// A line present on both sides (` ` prefix).
    Context(String),
    /// A line removed from the old side (`-` prefix).
    Remove(String),
    /// A line added on the new side (`+` prefix).
    Add(String),
}

/// A parsed hunk from a unified diff with raw line ordering preserved.
#[derive(Clone, Debug)]
pub(super) struct ParsedHunk {
    /// 1-based old start line from the `@@ -a,b +c,d @@` header.
    pub(super) old_start: usize,
    /// Text after the second `@@` (e.g. `fn process_event`), if any.
    pub(super) semantic_anchor: Option<String>,
    /// The hunk's diff lines in raw order.
    pub(super) lines: Vec<DiffLine>,
    /// A `\ No newline at end of file` marker followed a `-` or context line:
    /// the OLD side's final line in this hunk has no trailing newline.
    pub(super) old_missing_newline: bool,
    /// A `\ No newline at end of file` marker followed a `+` or context line:
    /// the NEW side's final line in this hunk must have no trailing newline.
    pub(super) new_missing_newline: bool,
}

impl ParsedHunk {
    /// The hunk's old-side lines (context + removals) in order.
    pub(super) fn old_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                DiffLine::Context(s) | DiffLine::Remove(s) => Some(s.as_str()),
                DiffLine::Add(_) => None,
            })
            .collect()
    }

    /// The hunk's new-side lines (context + additions) in order.
    pub(super) fn new_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                DiffLine::Context(s) | DiffLine::Add(s) => Some(s.as_str()),
                DiffLine::Remove(_) => None,
            })
            .collect()
    }

    /// Number of old-side lines (context + removals).
    pub(super) fn old_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Context(_) | DiffLine::Remove(_)))
            .count()
    }

    /// Number of new-side lines (context + additions).
    pub(super) fn new_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Context(_) | DiffLine::Add(_)))
            .count()
    }

    /// Number of added lines.
    pub(super) fn add_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Add(_)))
            .count()
    }

    /// Number of removed lines.
    pub(super) fn remove_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Remove(_)))
            .count()
    }
}

/// Extract the semantic anchor — the text after the second `@@` in a unified
/// diff hunk header. For `@@ -10,5 +10,6 @@ fn process_event` this returns
/// `Some("fn process_event")`. Returns `None` when there is no second `@@` or
/// the text after it is empty or whitespace-only.
///
/// Uses a forward scan (first `@@`, then the next `@@` in the remainder)
/// rather than `rfind`, so an anchor that itself contains `@@` is handled.
pub(super) fn parse_semantic_anchor(header: &str) -> Option<String> {
    let first = header.find("@@")?;
    let rest = header.get(first + 2..)?;
    let off = rest.find("@@")?;
    let after = rest.get(off + 2..)?;
    let trimmed = after.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The hunk header shape the parser requires, quoted in error messages so a
/// failing caller learns the fix without consulting external docs.
const HUNK_HEADER_FORM: &str = "@@ -<old_start>[,<old_count>] +<new_start>[,<new_count>] @@";

/// Parse the `@@` header line to extract the 1-based old start line and the
/// semantic anchor.
pub(super) fn parse_hunk_header(line: &str) -> Result<(usize, Option<String>), String> {
    let stripped = line.trim_start_matches('@').trim_start();
    let old_part = stripped.strip_prefix('-').ok_or_else(|| {
        format!("malformed hunk header: expected `{HUNK_HEADER_FORM}`, got `{line}`")
    })?;
    let start_str = old_part.split([',', ' ']).next().unwrap_or("1");
    let old_start = start_str.parse::<usize>().map_err(|e| {
        format!(
            "bad start line in hunk header `{line}`: {e}; expected `{HUNK_HEADER_FORM}` \
             with a numeric <old_start>"
        )
    })?;

    Ok((old_start, parse_semantic_anchor(line)))
}

/// Record a `\ No newline at end of file` marker against the diff line that
/// precedes it. After a removal it marks the OLD side's final line; after an
/// addition the NEW side's; after a context line, both (the line is the final
/// line of both files).
fn record_newline_marker(hunk: &mut ParsedHunk, marker_line: &str) -> Result<(), String> {
    let Some(prev) = hunk.lines.last() else {
        return Err(format!(
            "'{marker_line}' marker without a preceding diff line: it must directly follow \
             the context (' '), removal ('-'), or addition ('+') line it annotates, not open \
             a hunk"
        ));
    };
    match prev {
        DiffLine::Context(_) => {
            hunk.old_missing_newline = true;
            hunk.new_missing_newline = true;
        }
        DiffLine::Remove(_) => hunk.old_missing_newline = true,
        DiffLine::Add(_) => hunk.new_missing_newline = true,
    }
    Ok(())
}

/// Parse a single-file unified diff block into a sequence of hunks.
pub(super) fn parse_unified_hunks(block: &str) -> Result<Vec<ParsedHunk>, String> {
    let mut hunks = Vec::new();
    let mut current: Option<ParsedHunk> = None;

    for line in block.lines() {
        // File headers appear only before the first hunk. Inside a hunk,
        // `--- x` is a *removal* whose removed content starts with `-- `
        // (e.g. a SQL comment) and `+++ x` an addition starting with `++ ` —
        // they must flow into the normal prefix handling below.
        if current.is_none() && (line.starts_with("--- ") || line.starts_with("+++ ")) {
            continue;
        }
        if line.starts_with("@@") {
            if let Some(h) = current.take() {
                hunks.push(h);
            }
            let (old_start, anchor) = parse_hunk_header(line)?;
            current = Some(ParsedHunk {
                old_start,
                semantic_anchor: anchor,
                lines: Vec::new(),
                old_missing_newline: false,
                new_missing_newline: false,
            });
            continue;
        }
        if let Some(ref mut hunk) = current {
            if line.starts_with('\\') {
                // `\ No newline at end of file`: a marker about the preceding
                // line, never content (content lines carry ' ', '-', or '+').
                record_newline_marker(hunk, line)?;
            } else if let Some(content) = line.strip_prefix('+') {
                hunk.lines.push(DiffLine::Add(content.to_string()));
            } else if let Some(content) = line.strip_prefix('-') {
                hunk.lines.push(DiffLine::Remove(content.to_string()));
            } else {
                let content = line.strip_prefix(' ').unwrap_or(line);
                hunk.lines.push(DiffLine::Context(content.to_string()));
            }
        }
    }
    if let Some(h) = current {
        hunks.push(h);
    }
    Ok(hunks)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parse_hunk_header_extracts_start_and_anchor() {
        let (start, anchor) = parse_hunk_header("@@ -10,5 +10,5 @@ fn process_event").unwrap();
        assert_eq!(start, 10);
        assert_eq!(anchor.as_deref(), Some("fn process_event"));
    }

    #[test]
    fn parse_hunk_header_no_anchor() {
        let (start, anchor) = parse_hunk_header("@@ -1,3 +1,3 @@").unwrap();
        assert_eq!(start, 1);
        assert!(anchor.is_none());
    }

    #[test]
    fn bare_at_at_header_error_teaches_expected_form() {
        let err = parse_hunk_header("@@").unwrap_err();
        assert_eq!(
            err,
            "malformed hunk header: expected `@@ -<old_start>[,<old_count>] \
             +<new_start>[,<new_count>] @@`, got `@@`"
        );
    }

    #[test]
    fn bare_at_at_header_in_block_surfaces_teaching_error() {
        let block = "\
--- a/f.txt
+++ b/f.txt
@@
-old
+new
";
        let err = parse_unified_hunks(block).unwrap_err();
        assert!(
            err.contains("expected `@@ -<old_start>[,<old_count>] +<new_start>[,<new_count>] @@`"),
            "{err}"
        );
        assert!(err.contains("got `@@`"), "{err}");
    }

    #[test]
    fn non_numeric_start_error_names_line_and_expected_form() {
        let err = parse_hunk_header("@@ -abc,3 +1,3 @@").unwrap_err();
        assert!(err.contains("`@@ -abc,3 +1,3 @@`"), "{err}");
        assert!(
            err.contains("expected `@@ -<old_start>[,<old_count>] +<new_start>[,<new_count>] @@`"),
            "{err}"
        );
        assert!(err.contains("numeric <old_start>"), "{err}");
    }

    #[test]
    fn semantic_anchor_after_second_at() {
        assert_eq!(
            parse_semantic_anchor("@@ -10,5 +10,6 @@ fn process_event").as_deref(),
            Some("fn process_event")
        );
    }

    #[test]
    fn semantic_anchor_absent() {
        assert!(parse_semantic_anchor("@@ -1,3 +1,4 @@").is_none());
    }

    #[test]
    fn semantic_anchor_whitespace_only_is_none() {
        assert!(parse_semantic_anchor("@@ -1,3 +1,4 @@  ").is_none());
    }

    #[test]
    fn semantic_anchor_multi_word() {
        assert_eq!(
            parse_semantic_anchor("@@ -1,3 +1,4 @@ impl Display for Foo").as_deref(),
            Some("impl Display for Foo")
        );
    }

    #[test]
    fn marker_after_remove_sets_old_side_only() {
        let block = "\
--- a/f.txt
+++ b/f.txt
@@ -2,1 +2,2 @@
-beta
\\ No newline at end of file
+beta
+gamma
";
        let hunks = parse_unified_hunks(block).unwrap();
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].old_missing_newline);
        assert!(!hunks[0].new_missing_newline);
        assert_eq!(hunks[0].old_lines(), vec!["beta"]);
        assert_eq!(hunks[0].new_lines(), vec!["beta", "gamma"]);
    }

    #[test]
    fn marker_after_add_sets_new_side_only() {
        let block = "\
--- a/f.txt
+++ b/f.txt
@@ -2,1 +2,1 @@
-beta
+BETA
\\ No newline at end of file
";
        let hunks = parse_unified_hunks(block).unwrap();
        assert!(!hunks[0].old_missing_newline);
        assert!(hunks[0].new_missing_newline);
    }

    #[test]
    fn marker_after_context_sets_both_sides() {
        let block = "\
--- a/f.txt
+++ b/f.txt
@@ -1,3 +1,3 @@
 alpha
-beta
+BETA
 omega
\\ No newline at end of file
";
        let hunks = parse_unified_hunks(block).unwrap();
        assert!(hunks[0].old_missing_newline);
        assert!(hunks[0].new_missing_newline);
        assert_eq!(hunks[0].old_lines(), vec!["alpha", "beta", "omega"]);
    }

    #[test]
    fn markers_on_both_sides_set_both_flags() {
        let block = "\
--- a/f.txt
+++ b/f.txt
@@ -1,2 +1,2 @@
 alpha
-beta
\\ No newline at end of file
+BETA
\\ No newline at end of file
";
        let hunks = parse_unified_hunks(block).unwrap();
        assert!(hunks[0].old_missing_newline);
        assert!(hunks[0].new_missing_newline);
        assert_eq!(hunks[0].old_lines(), vec!["alpha", "beta"]);
        assert_eq!(hunks[0].new_lines(), vec!["alpha", "BETA"]);
    }

    #[test]
    fn marker_without_preceding_line_is_an_error() {
        let block = "\
--- a/f.txt
+++ b/f.txt
@@ -1,1 +1,1 @@
\\ No newline at end of file
";
        let err = parse_unified_hunks(block).unwrap_err();
        assert!(err.contains("without a preceding diff line"), "{err}");
        assert!(err.contains("must directly follow"), "{err}");
    }

    #[test]
    fn no_marker_leaves_flags_unset() {
        let block = "\
--- a/f.txt
+++ b/f.txt
@@ -1,1 +1,1 @@
-a
+b
";
        let hunks = parse_unified_hunks(block).unwrap();
        assert!(!hunks[0].old_missing_newline);
        assert!(!hunks[0].new_missing_newline);
    }
}
