//! Parser and applicator for Claude Code's `*** Begin Patch` format.
//!
//! Claude Code uses a simplified patch format where `*** Begin Patch` /
//! `*** End Patch` delimit the patch, `*** Update File: <path>` names
//! each target, and `@@` hunks use context-based string matching rather
//! than line-number-based offsets. Models trained on Claude Code output
//! frequently produce this format by default.
//!
//! Hunks are parsed with their interleaving preserved: the canonical
//! `ctx / -old / +new / ctx / -old / +new` shape forms one match window
//! (context **and** removal lines, in order) and one replacement (context
//! **and** addition lines, in order). The optional locator text after `@@`
//! (e.g. `@@ fn process_event`) scopes the search: when the window matches
//! more than once, the candidate closest after the anchor line wins; with
//! no usable anchor, multiple matches are refused rather than guessed.

use super::patch_match::{AmbiguousMatch, find_all_context_matches};

/// One line of a Claude Code hunk, preserving interleaving order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CcLine {
    /// Unchanged context line (part of the match window and the
    /// replacement).
    Context(String),
    /// Removed line (part of the match window only).
    Remove(String),
    /// Added line (part of the replacement only).
    Add(String),
}

/// A single hunk in Claude Code format.
#[derive(Clone, Debug)]
pub(super) struct CcHunk {
    /// Locator text following `@@` (e.g. a function signature), used to
    /// scope the context search. `None` for a bare `@@` line.
    pub anchor: Option<String>,
    /// Hunk body in original order.
    pub lines: Vec<CcLine>,
}

impl CcHunk {
    /// The match window: context and removal lines in order.
    fn old_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                CcLine::Context(s) | CcLine::Remove(s) => Some(s.as_str()),
                CcLine::Add(_) => None,
            })
            .collect()
    }

    /// The replacement: context and addition lines in order.
    fn new_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                CcLine::Context(s) | CcLine::Add(s) => Some(s.as_str()),
                CcLine::Remove(_) => None,
            })
            .collect()
    }

    /// Number of added lines.
    pub(super) fn addition_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, CcLine::Add(_)))
            .count()
    }

    /// Number of removed lines.
    pub(super) fn removal_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, CcLine::Remove(_)))
            .count()
    }
}

/// A parsed per-file entry from a Claude Code format patch.
pub(super) struct CcFileBlock {
    /// Target file path from the `*** Update File:` line.
    pub target: String,
    /// Hunks to apply, in order.
    pub hunks: Vec<CcHunk>,
}

/// Detects whether the input uses Claude Code's `*** Begin Patch` format.
pub(super) fn is_claude_code_format(input: &str) -> bool {
    input.trim_start().starts_with("*** Begin Patch")
}

/// Parses a Claude Code format patch into per-file [`CcFileBlock`] records.
pub(super) fn parse_claude_code(input: &str) -> Result<Vec<CcFileBlock>, String> {
    let mut blocks: Vec<CcFileBlock> = Vec::new();
    let mut current_target: Option<String> = None;
    let mut current_hunks: Vec<CcHunk> = Vec::new();
    let mut current_hunk: Option<CcHunk> = None;

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed == "*** Begin Patch" || trimmed == "*** End Patch" {
            continue;
        }

        if let Some(unsupported) = trimmed
            .strip_prefix("*** Add File: ")
            .or_else(|| trimmed.strip_prefix("*** Delete File: "))
        {
            return Err(format!(
                "*** Add File / *** Delete File are not supported. \
                 Use the write or bash tool to create or delete files. \
                 (found: {unsupported})"
            ));
        }

        if let Some(path) = trimmed.strip_prefix("*** Update File: ") {
            if let Some(target) = current_target.take() {
                if let Some(hunk) = current_hunk.take() {
                    current_hunks.push(hunk);
                }
                blocks.push(CcFileBlock {
                    target,
                    hunks: std::mem::take(&mut current_hunks),
                });
            }
            current_target = Some(path.trim().to_string());
            current_hunk = None;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("@@") {
            if let Some(hunk) = current_hunk.take() {
                current_hunks.push(hunk);
            }
            let anchor = rest.trim();
            current_hunk = Some(CcHunk {
                anchor: (!anchor.is_empty()).then(|| anchor.to_string()),
                lines: Vec::new(),
            });
            continue;
        }

        if let Some(ref mut hunk) = current_hunk {
            if let Some(added) = line.strip_prefix('+') {
                hunk.lines.push(CcLine::Add(added.to_string()));
            } else if let Some(removed) = line.strip_prefix('-') {
                hunk.lines.push(CcLine::Remove(removed.to_string()));
            } else {
                let ctx_line = line.strip_prefix(' ').unwrap_or(line).to_string();
                hunk.lines.push(CcLine::Context(ctx_line));
            }
        }
    }

    if let Some(target) = current_target.take() {
        if let Some(hunk) = current_hunk.take() {
            current_hunks.push(hunk);
        }
        blocks.push(CcFileBlock {
            target,
            hunks: std::mem::take(&mut current_hunks),
        });
    }

    if blocks.is_empty() {
        return Err(
            "patch uses *** Begin Patch format but contained no *** Update File entries"
                .to_string(),
        );
    }
    Ok(blocks)
}

/// Selects the match position for `positions`, using the hunk's `@@` anchor
/// to scope when there are several candidates.
///
/// With an anchor that occurs in the file, each candidate is scored by its
/// distance below the nearest anchor occurrence at or above it (Claude Code
/// anchors precede the hunk content, mirroring git hunk headers); the
/// closest candidate wins when unique. Without a usable anchor, multiple
/// candidates are ambiguous and refused.
fn select_cc_position(
    positions: &[usize],
    content_lines: &[&str],
    anchor: Option<&str>,
) -> Result<usize, AmbiguousMatch> {
    if let [only] = positions {
        return Ok(*only);
    }

    if let Some(anchor) = anchor {
        let anchor_trimmed = anchor.trim();
        let anchor_lines: Vec<usize> = content_lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains(anchor_trimmed))
            .map(|(i, _)| i)
            .collect();
        if !anchor_lines.is_empty() {
            // Distance from the nearest anchor at or above the candidate;
            // candidates with no preceding anchor are not considered.
            let score = |pos: usize| -> Option<usize> {
                anchor_lines
                    .iter()
                    .filter(|&&a| a <= pos)
                    .map(|&a| pos - a)
                    .min()
            };
            let scored: Vec<(usize, usize)> = positions
                .iter()
                .filter_map(|&p| score(p).map(|s| (p, s)))
                .collect();
            if let Some(&(best_pos, best_score)) = scored.iter().min_by_key(|&&(_, s)| s)
                && scored.iter().filter(|&&(_, s)| s == best_score).count() == 1
            {
                return Ok(best_pos);
            }
        }
    }

    Err(AmbiguousMatch {
        candidate_lines: positions.iter().map(|p| p + 1).collect(),
    })
}

/// Applies a single Claude Code hunk to file content using string matching.
///
/// The match window is the hunk's context **and** removal lines in their
/// original interleaved order; the replacement substitutes the context and
/// addition lines in order, so context between change runs is preserved.
pub(super) fn apply_cc_hunk(content: &str, hunk: &CcHunk) -> Result<String, String> {
    let search_lines = hunk.old_lines();

    if search_lines.is_empty() && hunk.addition_count() > 0 {
        let mut result = content.to_string();
        if !result.ends_with('\n') {
            result.push('\n');
        }
        for line in hunk.new_lines() {
            result.push_str(line);
            result.push('\n');
        }
        return Ok(result);
    }

    if search_lines.is_empty() {
        return Ok(content.to_string());
    }

    let content_lines: Vec<&str> = content.lines().collect();
    let search_len = search_lines.len();

    if search_len > content_lines.len() {
        return Err(format!(
            "search pattern ({search_len} lines) is longer than file ({} lines). Looking for:\n{}",
            content_lines.len(),
            preview(&search_lines),
        ));
    }

    let Some((positions, _kind)) = find_all_context_matches(&content_lines, &search_lines) else {
        return Err(format!(
            "could not find context in file. Looking for:\n{}",
            preview(&search_lines),
        ));
    };

    let pos = select_cc_position(&positions, &content_lines, hunk.anchor.as_deref()).map_err(
        |ambiguous| {
            format!(
                "context matches at multiple locations ({}){}; refusing to apply rather than \
                 guess. Add more surrounding context or an `@@ <anchor>` locator naming the \
                 enclosing declaration",
                ambiguous.describe_candidates(),
                hunk.anchor.as_deref().map_or_else(String::new, |a| format!(
                    " and the anchor `{a}` does not disambiguate them"
                )),
            )
        },
    )?;

    let new_lines = hunk.new_lines();
    let mut result_lines: Vec<&str> = Vec::with_capacity(content_lines.len());
    result_lines.extend_from_slice(&content_lines[..pos]);
    result_lines.extend(new_lines.iter().copied());
    let after_match = pos + search_len;
    if after_match < content_lines.len() {
        result_lines.extend_from_slice(&content_lines[after_match..]);
    }

    let mut result = result_lines.join("\n");
    if content.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

/// First three lines of the search window for error messages.
fn preview(search_lines: &[&str]) -> String {
    search_lines
        .iter()
        .take(3)
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Applies all Claude Code hunks to file content sequentially.
pub(super) fn apply_cc_hunks(content: &str, hunks: &[CcHunk]) -> Result<String, String> {
    let mut current = content.to_string();
    for (i, hunk) in hunks.iter().enumerate() {
        current =
            apply_cc_hunk(&current, hunk).map_err(|e| format!("hunk {} failed: {e}", i + 1))?;
    }
    Ok(current)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn hunk(anchor: Option<&str>, lines: Vec<CcLine>) -> CcHunk {
        CcHunk {
            anchor: anchor.map(str::to_string),
            lines,
        }
    }

    fn ctx(s: &str) -> CcLine {
        CcLine::Context(s.to_string())
    }
    fn rem(s: &str) -> CcLine {
        CcLine::Remove(s.to_string())
    }
    fn add(s: &str) -> CcLine {
        CcLine::Add(s.to_string())
    }

    #[test]
    fn detects_claude_code_format() {
        assert!(is_claude_code_format("*** Begin Patch\n*** End Patch"));
        assert!(is_claude_code_format("  *** Begin Patch\n*** End Patch"));
        assert!(!is_claude_code_format("--- a/file.rs\n+++ b/file.rs\n"));
    }

    #[test]
    fn parses_single_file_single_hunk() {
        let input = "\
*** Begin Patch
*** Update File: /tmp/test.rs
@@
 fn main() {
     let x = 1;
+    let y = 2;
 }
*** End Patch";
        let blocks = parse_claude_code(input).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].target, "/tmp/test.rs");
        assert_eq!(blocks[0].hunks.len(), 1);
        assert_eq!(
            blocks[0].hunks[0].lines,
            vec![
                ctx("fn main() {"),
                ctx("    let x = 1;"),
                add("    let y = 2;"),
                ctx("}"),
            ]
        );
        assert!(blocks[0].hunks[0].anchor.is_none());
    }

    #[test]
    fn parses_interleaved_hunk_preserving_order() {
        // The canonical ctx / -old / +new / ctx / -old / +new shape: the
        // historical parser discarded interleaved context, breaking the
        // match window. The order must be preserved exactly.
        let input = "\
*** Begin Patch
*** Update File: test.rs
@@ fn main
 fn main() {
-    let x = 1;
+    let x = 2;
     println!(\"x\");
-    let y = 1;
+    let y = 2;
 }
*** End Patch";
        let blocks = parse_claude_code(input).unwrap();
        let h = &blocks[0].hunks[0];
        assert_eq!(h.anchor.as_deref(), Some("fn main"));
        assert_eq!(
            h.lines,
            vec![
                ctx("fn main() {"),
                rem("    let x = 1;"),
                add("    let x = 2;"),
                ctx("    println!(\"x\");"),
                rem("    let y = 1;"),
                add("    let y = 2;"),
                ctx("}"),
            ]
        );
        assert_eq!(h.addition_count(), 2);
        assert_eq!(h.removal_count(), 2);
    }

    /// H10 regression: the reproduced failing shape — interleaved context
    /// between change runs — must apply correctly end to end.
    #[test]
    fn applies_interleaved_hunk() {
        let content = "fn main() {\n    let x = 1;\n    println!(\"x\");\n    let y = 1;\n}\n";
        let input = "\
*** Begin Patch
*** Update File: test.rs
@@ fn main
 fn main() {
-    let x = 1;
+    let x = 2;
     println!(\"x\");
-    let y = 1;
+    let y = 2;
 }
*** End Patch";
        let blocks = parse_claude_code(input).unwrap();
        let result = apply_cc_hunks(content, &blocks[0].hunks).unwrap();
        assert_eq!(
            result,
            "fn main() {\n    let x = 2;\n    println!(\"x\");\n    let y = 2;\n}\n"
        );
    }

    #[test]
    fn parses_removal_and_addition() {
        let input = "\
*** Begin Patch
*** Update File: test.rs
@@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
*** End Patch";
        let blocks = parse_claude_code(input).unwrap();
        let h = &blocks[0].hunks[0];
        assert_eq!(h.removal_count(), 1);
        assert_eq!(h.addition_count(), 1);
        assert!(h.lines.contains(&rem("    let x = 1;")));
        assert!(h.lines.contains(&add("    let x = 2;")));
    }

    #[test]
    fn parses_multi_file() {
        let input = "\
*** Begin Patch
*** Update File: a.rs
@@
 fn a() {}
+fn b() {}
*** Update File: c.rs
@@
-old()
+new()
*** End Patch";
        let blocks = parse_claude_code(input).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].target, "a.rs");
        assert_eq!(blocks[1].target, "c.rs");
    }

    #[test]
    fn apply_hunk_inserts_line() {
        let content = "fn main() {\n    let x = 1;\n}\n";
        let h = hunk(
            None,
            vec![
                ctx("fn main() {"),
                ctx("    let x = 1;"),
                add("    let y = 2;"),
            ],
        );
        let result = apply_cc_hunk(content, &h).unwrap();
        assert!(result.contains("let y = 2;"));
        assert!(result.contains("let x = 1;"));
    }

    #[test]
    fn apply_hunk_replaces_line() {
        let content = "fn main() {\n    let x = 1;\n}\n";
        let h = hunk(
            None,
            vec![
                ctx("fn main() {"),
                rem("    let x = 1;"),
                add("    let x = 2;"),
            ],
        );
        let result = apply_cc_hunk(content, &h).unwrap();
        assert!(result.contains("let x = 2;"));
        assert!(!result.contains("let x = 1;"));
    }

    #[test]
    fn apply_hunk_context_not_found() {
        let content = "fn main() {}\n";
        let h = hunk(None, vec![ctx("nonexistent line"), add("added")]);
        let result = apply_cc_hunk(content, &h);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("could not find context"));
    }

    #[test]
    fn anchor_scopes_duplicate_windows() {
        // Two identical bodies under different functions; the anchor names
        // the second function, so the second body must be patched.
        let content = "\
fn alpha() {
    let v = 1;
}

fn beta() {
    let v = 1;
}
";
        let h = hunk(
            Some("fn beta"),
            vec![rem("    let v = 1;"), add("    let v = 2;")],
        );
        let result = apply_cc_hunk(content, &h).unwrap();
        assert_eq!(
            result,
            "fn alpha() {\n    let v = 1;\n}\n\nfn beta() {\n    let v = 2;\n}\n"
        );
    }

    #[test]
    fn duplicate_windows_without_anchor_are_refused() {
        let content = "fn a() {\n    let v = 1;\n}\nfn b() {\n    let v = 1;\n}\n";
        let h = hunk(None, vec![rem("    let v = 1;"), add("    let v = 2;")]);
        let err = apply_cc_hunk(content, &h).unwrap_err();
        assert!(err.contains("multiple locations"), "{err}");
        assert!(err.contains("refusing"), "{err}");
    }

    #[test]
    fn duplicate_windows_with_unmatched_anchor_are_refused() {
        let content = "fn a() {\n    let v = 1;\n}\nfn b() {\n    let v = 1;\n}\n";
        let h = hunk(
            Some("fn missing"),
            vec![rem("    let v = 1;"), add("    let v = 2;")],
        );
        let err = apply_cc_hunk(content, &h).unwrap_err();
        assert!(err.contains("multiple locations"), "{err}");
    }

    #[test]
    fn preserves_missing_trailing_newline() {
        let content = "line one\nline two";
        let h = hunk(None, vec![rem("line two"), add("line 2")]);
        let result = apply_cc_hunk(content, &h).unwrap();
        assert_eq!(result, "line one\nline 2");
    }

    #[test]
    fn empty_input_errors() {
        let result = parse_claude_code("*** Begin Patch\n*** End Patch");
        assert!(result.is_err());
    }
}
