//! Patch parsing — block splitting, header extraction, format detection.
//!
//! Extracted from `patch.rs` to stay under the 500-line rule. Contains
//! the pure parsing logic that turns raw patch text into structured
//! [`PatchBlock`] records. No async, no filesystem access.

use std::path::{Path, PathBuf};

/// The mutation kind a single per-file patch block represents.
///
/// Determined during block parsing by inspecting the `---`/`+++` headers
/// for `/dev/null` markers. Drives lifecycle behaviour: `Create` skips the
/// read-before-edit and file-exists gates, `Delete` skips AST validation
/// and runs after all writes/creates have committed, `Modify` is the
/// classic in-place patch behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PatchBlockKind {
    /// In-place modification of an existing file.
    Modify,
    /// Creation of a new file (source header was `/dev/null`).
    Create,
    /// Deletion of an existing file (target header was `/dev/null`).
    Delete,
}

/// Describes how the patch was parsed so `execute` can apply it correctly.
pub(super) enum PatchFormat<'a> {
    /// Standard unified diff — apply via `diffy`.
    UnifiedDiff { raw: &'a str },
    /// Claude Code format — apply via context-based string matching.
    ClaudeCode { hunks: Vec<super::patch_cc::CcHunk> },
}

/// A single per-file patch block.
pub(super) struct PatchBlock<'a> {
    /// Real target file path (absolute or relative) — taken from the
    /// `+++` header for `Modify`/`Create`, from the `---` header for
    /// `Delete`.
    pub target: String,
    /// Mutation kind, decided at parse time from the `---`/`+++` headers.
    pub kind: PatchBlockKind,
    /// How to apply this block.
    pub format: PatchFormat<'a>,
}

/// Splits a unified diff string into per-file blocks. The diffy 0.5
/// `Patch::from_str` parser only handles one block, so we split at every
/// *file header*: a line starting with `--- ` whose **next line** starts
/// with `+++ `.
///
/// Requiring the `+++` companion line distinguishes real headers from diff
/// removal lines whose removed content itself starts with `-- ` (SQL/Lua/
/// Haskell comments): removing `-- legacy comment` renders as
/// `--- legacy comment`, which a naive `\n--- ` split would misparse as a
/// new file block.
pub(super) fn split_blocks(input: &str) -> Vec<&str> {
    // Byte offsets of every line that starts a file block.
    let mut header_offsets: Vec<usize> = Vec::new();
    let mut prev: Option<(usize, &str)> = None;
    let mut offset = 0;
    for line in input.split_inclusive('\n') {
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        if let Some((prev_offset, prev_line)) = prev
            && prev_line.starts_with("--- ")
            && trimmed.starts_with("+++ ")
        {
            header_offsets.push(prev_offset);
        }
        prev = Some((offset, trimmed));
        offset += line.len();
    }

    let mut blocks = Vec::with_capacity(header_offsets.len());
    for (i, &start) in header_offsets.iter().enumerate() {
        let end = header_offsets.get(i + 1).copied().unwrap_or(input.len());
        blocks.push(&input[start..end]);
    }
    blocks
}

pub(super) fn strip_git_prefix(path: &str) -> &str {
    if let Some(rest) = path.strip_prefix("a/") {
        rest
    } else if let Some(rest) = path.strip_prefix("b/") {
        rest
    } else {
        path
    }
}

/// `true` if a header path refers to `/dev/null` (either literally or as
/// the git-prefixed form `a/dev/null` / `b/dev/null`).
pub(super) fn is_dev_null(raw_path: &str) -> bool {
    raw_path == "/dev/null" || strip_git_prefix(raw_path) == "dev/null"
}

/// Scans both `---` and `+++` headers in a block and decides the
/// [`PatchBlockKind`].
///
/// Returns the real target path (the side that is not `/dev/null`) and
/// the block kind. `Create` is detected from `--- /dev/null`; `Delete`
/// is detected from `+++ /dev/null`. Rejects blocks where both sides
/// are `/dev/null` (no-op) or where either header is missing.
pub(super) fn extract_headers(block: &str) -> Result<(String, PatchBlockKind), String> {
    let mut source: Option<&str> = None;
    let mut target: Option<&str> = None;
    for line in block.lines() {
        if source.is_none()
            && let Some(rest) = line.strip_prefix("--- ")
        {
            source = Some(rest.split_whitespace().next().unwrap_or(""));
        } else if target.is_none()
            && let Some(rest) = line.strip_prefix("+++ ")
        {
            target = Some(rest.split_whitespace().next().unwrap_or(""));
        }
        if source.is_some() && target.is_some() {
            break;
        }
    }

    let src = source.ok_or_else(|| "missing --- header".to_string())?;
    let tgt = target.ok_or_else(|| "missing +++ header".to_string())?;

    let src_devnull = is_dev_null(src);
    let tgt_devnull = is_dev_null(tgt);

    if src_devnull && tgt_devnull {
        return Err("both --- and +++ headers are /dev/null".to_string());
    }

    if src_devnull {
        let path = strip_git_prefix(tgt).to_string();
        if path.is_empty() {
            return Err("empty +++ header".to_string());
        }
        return Ok((path, PatchBlockKind::Create));
    }

    if tgt_devnull {
        let path = strip_git_prefix(src).to_string();
        if path.is_empty() {
            return Err("empty --- header".to_string());
        }
        return Ok((path, PatchBlockKind::Delete));
    }

    let path = strip_git_prefix(tgt).to_string();
    if path.is_empty() {
        return Err("empty +++ header".to_string());
    }
    Ok((path, PatchBlockKind::Modify))
}

/// Parses the patch text into per-file [`PatchBlock`] records.
///
/// Automatically detects Claude Code format (`*** Begin Patch`) vs
/// standard unified diff (`---`/`+++` headers).
pub(super) fn parse_blocks(input: &str) -> Result<Vec<PatchBlock<'_>>, String> {
    if super::patch_cc::is_claude_code_format(input) {
        let cc_blocks = super::patch_cc::parse_claude_code(input)?;
        return Ok(cc_blocks
            .into_iter()
            .map(|b| PatchBlock {
                target: b.target,
                kind: PatchBlockKind::Modify,
                format: PatchFormat::ClaudeCode { hunks: b.hunks },
            })
            .collect());
    }

    let raw_blocks = split_blocks(input);
    let mut blocks = Vec::with_capacity(raw_blocks.len());
    for raw in raw_blocks {
        if raw.trim().is_empty() {
            continue;
        }
        let (target, kind) = extract_headers(raw)?;
        blocks.push(PatchBlock {
            target,
            kind,
            format: PatchFormat::UnifiedDiff { raw },
        });
    }
    if blocks.is_empty() {
        let has_star_markers = input.contains("*** ");
        let has_at_markers = input.contains("@@");
        let hint = if has_star_markers && !input.trim_start().starts_with("*** Begin Patch") {
            ". Input contains '***' markers — did you mean to use *** Begin Patch format? \
             The *** Begin Patch line must appear before any *** Update File lines"
        } else if has_at_markers && !input.contains("--- ") {
            ". Input has @@ markers but no --- header lines. \
             Unified diff requires --- a/path and +++ b/path headers before each @@ hunk"
        } else if input.contains("\\n") && !input.contains('\n') {
            ". Input appears to contain literal \\n characters instead of actual newlines"
        } else {
            ". Expected either unified diff (--- a/file / +++ b/file) or \
             Claude Code format (*** Begin Patch / *** Update File: path)"
        };
        return Err(format!("patch contained no file blocks{hint}"));
    }
    Ok(blocks)
}

pub(super) fn resolve_path(working_dir: &Path, target: &str) -> PathBuf {
    let p = Path::new(target);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        working_dir.join(p)
    }
}

/// Collects the `+`-prefixed lines from a unified-diff block into a
/// single string ready to be written to disk as a new file.
///
/// Used only for `PatchBlockKind::Create` blocks where the `---` header
/// is `/dev/null`. The three-tier resolver in `patch_apply` is bypassed
/// because there is no original content to match context against — the
/// patch should consist of a single `@@ -0,0 +1,N @@` hunk with only
/// additions. Context and removal lines are ignored defensively.
///
/// Returns `(content, hunk_count, lines_added)`.
pub(super) fn collect_unified_additions(raw: &str) -> (String, usize, usize) {
    let mut content = String::new();
    let mut hunks = 0usize;
    let mut added = 0usize;
    for line in raw.lines() {
        // File headers appear only before the first hunk; after that, a
        // `+++ x` line is an addition whose content starts with `++ `.
        if hunks == 0 && (line.starts_with("--- ") || line.starts_with("+++ ")) {
            continue;
        }
        if line.starts_with("@@") {
            hunks = hunks.saturating_add(1);
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            content.push_str(rest);
            content.push('\n');
            added = added.saturating_add(1);
        }
    }
    (content, hunks, added)
}
