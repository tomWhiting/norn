//! Parser and applicator for Claude Code's `*** Begin Patch` format.
//!
//! Claude Code uses a simplified patch format where `*** Begin Patch` /
//! `*** End Patch` delimit the patch, `*** Update File: <path>` names
//! each target, and `@@` hunks use context-based string matching rather
//! than line-number-based offsets. Models trained on Claude Code output
//! frequently produce this format by default.

/// A single hunk in Claude Code format: context lines locate the
/// position, `-` lines are removed, `+` lines are inserted.
pub(super) struct CcHunk {
    pub context_before: Vec<String>,
    pub removals: Vec<String>,
    pub additions: Vec<String>,
}

/// A parsed per-file entry from a Claude Code format patch.
pub(super) struct CcFileBlock {
    pub target: String,
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
    let mut in_hunk = false;
    let mut ctx_before: Vec<String> = Vec::new();
    let mut removes: Vec<String> = Vec::new();
    let mut adds: Vec<String> = Vec::new();
    let mut seen_changes = false;

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
                if in_hunk {
                    current_hunks.push(finish_hunk(&mut ctx_before, &mut removes, &mut adds));
                }
                blocks.push(CcFileBlock {
                    target,
                    hunks: std::mem::take(&mut current_hunks),
                });
            }
            current_target = Some(path.trim().to_string());
            in_hunk = false;
            ctx_before.clear();
            removes.clear();
            adds.clear();
            seen_changes = false;
            continue;
        }

        if trimmed.starts_with("@@") {
            if in_hunk {
                current_hunks.push(finish_hunk(&mut ctx_before, &mut removes, &mut adds));
            }
            in_hunk = true;
            ctx_before.clear();
            removes.clear();
            adds.clear();
            seen_changes = false;
            continue;
        }

        if in_hunk {
            if let Some(added) = line.strip_prefix('+') {
                adds.push(added.to_string());
                seen_changes = true;
            } else if let Some(removed) = line.strip_prefix('-') {
                removes.push(removed.to_string());
                seen_changes = true;
            } else if !seen_changes {
                let ctx_line = line.strip_prefix(' ').unwrap_or(line).to_string();
                ctx_before.push(ctx_line);
            }
        }
    }

    if let Some(target) = current_target.take() {
        if in_hunk {
            current_hunks.push(finish_hunk(&mut ctx_before, &mut removes, &mut adds));
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

fn finish_hunk(
    ctx_before: &mut Vec<String>,
    removes: &mut Vec<String>,
    adds: &mut Vec<String>,
) -> CcHunk {
    CcHunk {
        context_before: std::mem::take(ctx_before),
        removals: std::mem::take(removes),
        additions: std::mem::take(adds),
    }
}

/// Applies a single Claude Code hunk to file content using string matching.
pub(super) fn apply_cc_hunk(content: &str, hunk: &CcHunk) -> Result<String, String> {
    let search_lines: Vec<&str> = hunk
        .context_before
        .iter()
        .chain(hunk.removals.iter())
        .map(String::as_str)
        .collect();

    if search_lines.is_empty() && !hunk.additions.is_empty() {
        let mut result = content.to_string();
        if !result.ends_with('\n') {
            result.push('\n');
        }
        for line in &hunk.additions {
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
        let preview = search_lines
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "search pattern ({search_len} lines) is longer than file ({} lines). Looking for:\n{preview}",
            content_lines.len()
        ));
    }

    let mut match_pos = None;
    for i in 0..=content_lines.len() - search_len {
        let window = &content_lines[i..i + search_len];
        if window
            .iter()
            .zip(search_lines.iter())
            .all(|(a, b)| a.trim_end() == b.trim_end())
        {
            match_pos = Some(i);
            break;
        }
    }

    let pos = match_pos.ok_or_else(|| {
        let preview = search_lines
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        format!("could not find context in file. Looking for:\n{preview}")
    })?;

    let mut result_lines: Vec<&str> = Vec::with_capacity(content_lines.len());
    result_lines.extend_from_slice(&content_lines[..pos]);
    for line in &hunk.context_before {
        result_lines.push(line.as_str());
    }
    for line in &hunk.additions {
        result_lines.push(line.as_str());
    }
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::missing_const_for_fn,
    clippy::unnecessary_trailing_comma
)]
mod tests {
    use super::*;

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
            blocks[0].hunks[0].context_before,
            vec!["fn main() {", "    let x = 1;"]
        );
        assert_eq!(blocks[0].hunks[0].additions, vec!["    let y = 2;"]);
        assert!(blocks[0].hunks[0].removals.is_empty());
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
        assert_eq!(blocks[0].hunks[0].removals, vec!["    let x = 1;"]);
        assert_eq!(blocks[0].hunks[0].additions, vec!["    let x = 2;"]);
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
        let hunk = CcHunk {
            context_before: vec!["fn main() {".to_string(), "    let x = 1;".to_string()],
            removals: vec![],
            additions: vec!["    let y = 2;".to_string()],
        };
        let result = apply_cc_hunk(content, &hunk).unwrap();
        assert!(result.contains("let y = 2;"));
        assert!(result.contains("let x = 1;"));
    }

    #[test]
    fn apply_hunk_replaces_line() {
        let content = "fn main() {\n    let x = 1;\n}\n";
        let hunk = CcHunk {
            context_before: vec!["fn main() {".to_string()],
            removals: vec!["    let x = 1;".to_string()],
            additions: vec!["    let x = 2;".to_string()],
        };
        let result = apply_cc_hunk(content, &hunk).unwrap();
        assert!(result.contains("let x = 2;"));
        assert!(!result.contains("let x = 1;"));
    }

    #[test]
    fn apply_hunk_context_not_found() {
        let content = "fn main() {}\n";
        let hunk = CcHunk {
            context_before: vec!["nonexistent line".to_string()],
            removals: vec![],
            additions: vec!["added".to_string()],
        };
        let result = apply_cc_hunk(content, &hunk);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("could not find context"));
    }

    #[test]
    fn empty_input_errors() {
        let result = parse_claude_code("*** Begin Patch\n*** End Patch");
        assert!(result.is_err());
    }
}
