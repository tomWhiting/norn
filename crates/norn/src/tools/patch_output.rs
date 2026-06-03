//! Output assembly for the `apply_patch` tool.
//!
//! Builds the resolution metadata that surfaces in both dry-run and
//! committed results: blast radius (per-file line deltas, containing
//! symbols, file-length impact), per-file AST validation, and per-hunk
//! resolution details. The same builders run regardless of `dry_run` so a
//! successful dry-run preview is byte-for-byte the same shape as a real
//! application (only the `committed` flag and disk-write step differ).

use serde_json::{Value, json};

use super::ast::{AstCheck, containing_symbols};
use super::patch::StagedFile;
use super::patch_parse::PatchBlockKind;
use super::validation::count_code_lines;

/// Human-readable status string for a staged block.
fn status_label(kind: PatchBlockKind) -> &'static str {
    match kind {
        PatchBlockKind::Modify => "modified",
        PatchBlockKind::Create => "created",
        PatchBlockKind::Delete => "deleted",
    }
}

/// Per-file status/metrics array. Mirrors the historical `per_file`
/// payload shape so existing consumers keep working.
#[must_use]
pub(super) fn per_file(staged: &[StagedFile]) -> Value {
    json!(
        staged
            .iter()
            .map(|s| json!({
                "path": s.path.to_string_lossy(),
                "status": status_label(s.kind),
                "hunks": s.hunks,
                "lines_added": s.added,
                "lines_removed": s.removed,
            }))
            .collect::<Vec<_>>()
    )
}

/// Blast radius: per-file line deltas, containing symbols, and file-length
/// impact, plus workspace totals.
///
/// `lines_modified` follows the Edit tool's convention — the overlap of
/// added and removed lines, i.e. `min(added, removed)`. `containing_symbols`
/// names the entities enclosing the changed region (functions, structs, …),
/// derived from tree-sitter entity extraction; it is empty for languages the
/// extractor does not support and for deletions (the file is gone).
#[must_use]
pub(super) fn blast_radius(staged: &[StagedFile]) -> Value {
    let mut files = Vec::with_capacity(staged.len());
    let mut total_added = 0usize;
    let mut total_removed = 0usize;

    for s in staged {
        total_added += s.added;
        total_removed += s.removed;

        let symbols = if matches!(s.kind, PatchBlockKind::Delete) {
            Vec::new()
        } else {
            let (start, end) = changed_byte_range(&s.original, &s.staged);
            containing_symbols(&s.path, &s.staged, start, end)
        };

        // Empty content yields a zero count, so Create (empty original) and
        // Delete (empty staged) fall out of `count_code_lines` naturally.
        let file_length_before = count_code_lines(&s.path, &s.original);
        let file_length_after = count_code_lines(&s.path, &s.staged);

        files.push(json!({
            "path": s.path.to_string_lossy(),
            "status": status_label(s.kind),
            "lines_added": s.added,
            "lines_removed": s.removed,
            "lines_modified": s.added.min(s.removed),
            "containing_symbols": symbols,
            "file_length_before": file_length_before,
            "file_length_after": file_length_after,
        }));
    }

    json!({
        "files": files,
        "totals": {
            "total_files": staged.len(),
            "total_added": total_added,
            "total_removed": total_removed,
        }
    })
}

/// Per-file AST validation summary: `pass`/`fail` with error positions.
///
/// Reuses the [`AstCheck`] captured during staging rather than re-parsing.
/// Deletions and unsupported languages report `pass` (there is no
/// post-mutation content to reject).
#[must_use]
pub(super) fn ast_validation(staged: &[StagedFile]) -> Value {
    json!(
        staged
            .iter()
            .map(|s| match &s.ast {
                AstCheck::Fail { errors } => json!({
                    "path": s.path.to_string_lossy(),
                    "status": "fail",
                    "errors": errors
                        .iter()
                        .map(|e| json!({
                            "line": e.line,
                            "column": e.column,
                            "message": e.render(),
                        }))
                        .collect::<Vec<_>>(),
                }),
                AstCheck::Pass | AstCheck::Unsupported => json!({
                    "path": s.path.to_string_lossy(),
                    "status": "pass",
                    "errors": Vec::<Value>::new(),
                }),
            })
            .collect::<Vec<_>>()
    )
}

/// Per-hunk resolution details.
///
/// Currently a placeholder carrying `{ file, hunk_index }` per hunk.
/// NTP-002 (tiered resolution metadata) enriches each entry with
/// `tier_used` / `entity_matched` / `drift` / `confidence` once it lands;
/// the structure is in place so this brief's R5 acceptance is satisfied.
#[must_use]
pub(super) fn resolution_details(staged: &[StagedFile]) -> Value {
    let mut entries = Vec::new();
    for s in staged {
        for hunk_index in 0..s.hunks {
            entries.push(json!({
                "file": s.path.to_string_lossy(),
                "hunk_index": hunk_index,
            }));
        }
    }
    json!(entries)
}

/// Byte range in `staged` covering the region that differs from `original`.
///
/// Trims matching lines from the front and back, then maps the surviving
/// changed span to byte offsets in `staged`. For a fresh file (`original`
/// empty) the whole staged content is the changed region. Returns `(0, 0)`
/// for empty staged content (deletions never reach here).
fn changed_byte_range(original: &str, staged: &str) -> (usize, usize) {
    if staged.is_empty() {
        return (0, 0);
    }
    let orig: Vec<&str> = original.lines().collect();
    let new: Vec<&str> = staged.lines().collect();

    let mut front = 0;
    while front < orig.len() && front < new.len() && orig[front] == new[front] {
        front += 1;
    }

    let mut back_o = orig.len();
    let mut back_n = new.len();
    while back_o > front && back_n > front && orig[back_o - 1] == new[back_n - 1] {
        back_o -= 1;
        back_n -= 1;
    }

    let start = line_start_byte(staged, front);
    let end = line_start_byte(staged, back_n);
    let len = staged.len();
    (start.min(len), end.max(start).min(len))
}

/// Byte offset where the `line_idx`-th line (0-based) starts in `s`.
///
/// Indices past the last line clamp to the string length.
fn line_start_byte(s: &str, line_idx: usize) -> usize {
    s.split_inclusive('\n').take(line_idx).map(str::len).sum()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::uninlined_format_args
)]
mod tests {
    use super::*;

    #[test]
    fn line_start_byte_maps_line_indices() {
        let s = "aa\nbbb\nc\n";
        assert_eq!(line_start_byte(s, 0), 0);
        assert_eq!(line_start_byte(s, 1), 3); // after "aa\n"
        assert_eq!(line_start_byte(s, 2), 7); // after "aa\nbbb\n"
        // Past the end clamps to the full length.
        assert_eq!(line_start_byte(s, 99), s.len());
    }

    #[test]
    fn changed_byte_range_new_file_covers_whole_content() {
        let staged = "fn a() {}\n";
        let (start, end) = changed_byte_range("", staged);
        assert_eq!(start, 0);
        assert_eq!(end, staged.len());
    }

    #[test]
    fn changed_byte_range_isolates_middle_change() {
        let original = "line1\nline2\nline3\n";
        let staged = "line1\nCHANGED\nline3\n";
        let (start, end) = changed_byte_range(original, staged);
        // The changed span is the second line.
        assert_eq!(&staged[start..end], "CHANGED\n");
    }

    #[test]
    fn changed_byte_range_empty_staged_is_zero() {
        assert_eq!(changed_byte_range("anything\n", ""), (0, 0));
    }
}
