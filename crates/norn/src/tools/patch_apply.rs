//! Mode-aware unified-diff application for `apply_patch`.
//!
//! Models frequently generate unified diffs with stale line numbers in the
//! `@@ -a,b +c,d @@` headers, but the `@@` semantic anchor (the text after
//! the second `@@`, e.g. `fn process_event`) and the hunk's context lines
//! are almost always correct. Rather than trusting line numbers first, the
//! default (`auto`) mode resolves each hunk through progressively weaker
//! signals (see `patch_resolve`):
//!
//! 1. **Entity-guided placement** — read the `@@` semantic anchor, use the
//!    injected [`EntityExtractor`] to find the named entity, and search for
//!    the hunk's context lines within that entity's line range.
//! 2. **Context-anchored search** — when there is no anchor or the entity
//!    is not found, search the whole file for the hunk's context lines.
//! 3. **Header-corrected diffy** — last resort: reconstruct a single-hunk
//!    unified diff with corrected counts and apply it via `diffy`.
//!
//! Strict and structural modes (see `patch_modes`) trade fallback for
//! precision. Each hunk's resolution is recorded in a
//! [`HunkResolution`] so callers can report which tier placed it, the entity
//! matched, and the line drift.

use std::path::Path;

use super::patch::PatchMode;
use super::patch_entity::EntityExtractor;
use super::patch_hunk::{ParsedHunk, parse_unified_hunks};
use super::patch_match::AmbiguousMatch;
use super::patch_modes::{resolve_hunk_strict, resolve_hunk_structural};
use super::patch_resolve::{HunkResolution, entity_range_for_anchor, resolve_hunk};
use crate::error::ToolError;

/// The result of applying a unified diff block to file content.
#[derive(Debug)]
pub(super) struct UnifiedPatchOutcome {
    /// The patched content (LF-delimited, always newline-terminated; the
    /// caller applies [`Self::trailing_newline`] and EOL restoration).
    pub(super) content: String,
    /// Hunks that actually applied (strict/structural failures excluded).
    pub(super) hunks_applied: usize,
    /// Lines added across applied hunks.
    pub(super) lines_added: usize,
    /// Lines removed across applied hunks.
    pub(super) lines_removed: usize,
    /// One [`HunkResolution`] per hunk, in patch order.
    pub(super) resolutions: Vec<HunkResolution>,
    /// Trailing-newline state declared by `\ No newline at end of file`
    /// markers on *applied* hunks: `Some(false)` when the new side's final
    /// line carries the marker, `Some(true)` when only the old side did (the
    /// patch replaces the unterminated final line with newline-terminated
    /// content), `None` when no applied hunk carried a marker (preserve the
    /// original file's state).
    pub(super) trailing_newline: Option<bool>,
}

/// Mode-aware application of a unified diff block to file content.
///
/// `mode` selects how each hunk is resolved:
///
/// * [`PatchMode::Auto`] — the entity-first tier order in `resolve_hunk`
///   (entity → context → diffy). A hunk that no tier can place aborts the whole
///   patch with [`ToolError::ExecutionFailed`], preserving the historical
///   all-or-nothing contract.
/// * [`PatchMode::Strict`] — exact stated-line matching only (translated by
///   the net line delta of hunks already applied). A hunk that does not match
///   exactly is left unapplied and recorded as a non-fatal failure carrying
///   its structural alternative; processing continues.
/// * [`PatchMode::Structural`] — entity resolution required per hunk, context
///   scoped to the entity, no diffy fallback. Failures are non-fatal as in
///   strict mode.
///
/// The returned [`UnifiedPatchOutcome`] counts only hunks that actually
/// applied, so strict/structural failures do not inflate the totals.
/// `extractor` drives entity resolution; passing `None` skips tier 1 in auto
/// mode and forces every structural-mode hunk to fail.
pub(super) fn apply_unified_tiered(
    raw: &str,
    original: &str,
    path: &Path,
    extractor: Option<&dyn EntityExtractor>,
    mode: PatchMode,
) -> Result<UnifiedPatchOutcome, ToolError> {
    let hunks = parse_unified_hunks(raw).map_err(|e| ToolError::ExecutionFailed {
        reason: format!("failed to parse patch for {}: {e}", path.display()),
    })?;
    if hunks.is_empty() {
        return Err(ToolError::ExecutionFailed {
            reason: format!("patch for {} contained no hunks", path.display()),
        });
    }

    let mut current = original.to_string();
    let mut resolutions: Vec<HunkResolution> = Vec::with_capacity(hunks.len());
    let mut cumulative_offset: i64 = 0;
    let mut hunks_applied = 0usize;
    let mut total_added = 0usize;
    let mut total_removed = 0usize;
    let mut trailing_newline: Option<bool> = None;

    for (i, hunk) in hunks.iter().enumerate() {
        // Resolve in a nested scope so the `file_lines` borrow of `current` is
        // released before `current` is reassigned below.
        let (applied_content, resolution) = {
            let file_lines: Vec<&str> = current.lines().collect();
            match mode {
                PatchMode::Auto => {
                    match resolve_hunk(
                        &current,
                        &file_lines,
                        hunk,
                        path,
                        cumulative_offset,
                        extractor,
                    ) {
                        Ok(Some((applied, resolution))) => (Some(applied), resolution),
                        Ok(None) => {
                            return Err(auto_resolution_error(i, hunk, &current, path, extractor));
                        }
                        Err(ambiguous) => {
                            return Err(ambiguous_resolution_error(i, hunk, path, &ambiguous));
                        }
                    }
                }
                PatchMode::Strict => resolve_hunk_strict(
                    &current,
                    &file_lines,
                    hunk,
                    path,
                    cumulative_offset,
                    extractor,
                ),
                PatchMode::Structural => resolve_hunk_structural(
                    &current,
                    &file_lines,
                    hunk,
                    path,
                    cumulative_offset,
                    extractor,
                ),
            }
        };

        if let Some(applied) = applied_content {
            cumulative_offset += i64::try_from(hunk.new_count()).unwrap_or(0)
                - i64::try_from(hunk.old_count()).unwrap_or(0);
            current = applied;
            hunks_applied += 1;
            total_added += hunk.add_count();
            total_removed += hunk.remove_count();
            // Newline markers only take effect when the hunk carrying them
            // actually applied: an unapplied strict/structural hunk must not
            // change the file's trailing-newline state.
            if hunk.new_missing_newline {
                trailing_newline = Some(false);
            } else if hunk.old_missing_newline {
                trailing_newline = Some(true);
            }
        }
        resolutions.push(resolution);
    }

    Ok(UnifiedPatchOutcome {
        content: current,
        hunks_applied,
        lines_added: total_added,
        lines_removed: total_removed,
        resolutions,
        trailing_newline,
    })
}

/// Build the structured refusal error for an auto-mode hunk whose context
/// matches several windows that the stated `@@` line cannot disambiguate.
/// Mirrors the Edit tool's ambiguity behaviour: list every candidate and
/// refuse rather than patching the first (or any) occurrence silently.
fn ambiguous_resolution_error(
    index: usize,
    hunk: &ParsedHunk,
    path: &Path,
    ambiguous: &AmbiguousMatch,
) -> ToolError {
    ToolError::ExecutionFailed {
        reason: format!(
            "hunk {} for {}: context matches at multiple locations ({}) and the stated @@ line {} \
             does not disambiguate them; refusing to apply rather than guess. Add more \
             surrounding context to the hunk or correct the @@ line to the intended occurrence.",
            index + 1,
            path.display(),
            ambiguous.describe_candidates(),
            hunk.old_start,
        ),
    }
}

/// Build the descriptive abort error for an auto-mode hunk that no tier could
/// place, mirroring the diagnostic the previous all-or-nothing path produced.
fn auto_resolution_error(
    index: usize,
    hunk: &ParsedHunk,
    current: &str,
    path: &Path,
    extractor: Option<&dyn EntityExtractor>,
) -> ToolError {
    let old_lines = hunk.old_lines();
    let preview: String = old_lines
        .iter()
        .take(3)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let anchor_note = match (extractor, hunk.semantic_anchor.as_deref()) {
        (Some(ext), Some(anchor)) => match entity_range_for_anchor(ext, current, path, anchor) {
            Some((es, ee)) => format!(
                " (entity '{anchor}' found at lines {}-{} but its context did not match within it)",
                es + 1,
                ee + 1,
            ),
            None => format!(" (entity '{anchor}' not found via entity extractor)"),
        },
        _ => String::new(),
    };
    ToolError::ExecutionFailed {
        reason: format!(
            "hunk {} for {}: context not found in file{anchor_note}. Looking for:\n{preview}",
            index + 1,
            path.display(),
        ),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::missing_const_for_fn
)]
mod tests {
    use super::super::patch_entity::ExtractedEntity;
    use super::*;
    use crate::tool::Confidence;

    /// Mock extractor returning a fixed entity list regardless of input, so
    /// tier-1 control flow is exercised independently of any real parser.
    struct MockExtractor {
        entities: Vec<ExtractedEntity>,
    }

    impl EntityExtractor for MockExtractor {
        fn extract(&self, _source: &str, _path: &Path) -> Option<Vec<ExtractedEntity>> {
            Some(self.entities.clone())
        }
    }

    /// Mock extractor that reports every language as unsupported (`None`),
    /// forcing tier-1 to be skipped even when an extractor is supplied.
    struct NoneExtractor;

    impl EntityExtractor for NoneExtractor {
        fn extract(&self, _source: &str, _path: &Path) -> Option<Vec<ExtractedEntity>> {
            None
        }
    }

    /// Build an [`ExtractedEntity`] with a 1-indexed inclusive line range,
    /// matching libyggd's `StructuralEntity` convention.
    fn entity(name: &str, line_start: usize, line_end: usize) -> ExtractedEntity {
        ExtractedEntity {
            name: Some(name.to_string()),
            qualified_name: name.to_string(),
            kind: "fn".to_string(),
            byte_range: 0..0,
            line_range: line_start..line_end,
        }
    }

    #[test]
    fn anchor_resolves_at_tier_1_with_extractor() {
        let original = "\
fn other() {
    let a = 0;
}

fn target() {
    let value = 1;
}
";
        // Stale line numbers (real `target` is at lines 5-7) but a correct
        // `@@ ... @@ fn target` anchor: the supplied extractor locates the
        // `target` entity, so tier 1 scopes the context search to its range.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -10,3 +10,3 @@ fn target
 fn target() {
-    let value = 1;
+    let value = 2;
 }
";
        let file_path = Path::new("file.rs");
        // `target` occupies lines 5-7 (1-indexed inclusive).
        let mock = MockExtractor {
            entities: vec![entity("other", 1, 3), entity("target", 5, 7)],
        };
        let extractor: Option<&dyn EntityExtractor> = Some(&mock);
        let out = apply_unified_tiered(diff_text, original, file_path, extractor, PatchMode::Auto)
            .unwrap();
        assert_eq!(out.resolutions.len(), 1);
        assert_eq!(out.resolutions[0].tier_used, 1);
        assert_eq!(
            out.resolutions[0].entity_matched.as_deref(),
            Some("fn target")
        );
        assert!(matches!(out.resolutions[0].confidence, Confidence::High));
        assert_eq!(out.resolutions[0].stated_line, 10);
        assert_eq!(out.resolutions[0].matched_at_line, Some(5));
        assert_eq!(out.resolutions[0].drift, -5);
        assert!(out.content.contains("let value = 2;"));
        assert!(!out.content.contains("let value = 1;"));
    }

    #[test]
    fn no_extractor_skips_tier_1() {
        let original = "\
fn other() {
    let a = 0;
}

fn target() {
    let value = 1;
}
";
        // The anchor names a real entity, but with no extractor tier 1 is
        // skipped entirely: resolution lands at tier 2 via full-file context
        // search instead of entity-scoped placement.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -10,3 +10,3 @@ fn target
 fn target() {
-    let value = 1;
+    let value = 2;
 }
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(out.resolutions.len(), 1);
        assert_eq!(out.resolutions[0].tier_used, 2);
        assert!(out.content.contains("let value = 2;"));
    }

    #[test]
    fn extractor_returning_none_forces_tier_2() {
        let original = "fn main() {\n    let x = 1;\n}\n";
        // An extractor is supplied, but it reports the language as
        // unsupported (`None`), so tier 1 is skipped and resolution falls to
        // the full-file context search even though the anchor is present.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@ fn main
 fn main() {
-    let x = 1;
+    let x = 2;
 }
";
        let file_path = Path::new("file.rs");
        let none = NoneExtractor;
        let extractor: Option<&dyn EntityExtractor> = Some(&none);
        let out = apply_unified_tiered(diff_text, original, file_path, extractor, PatchMode::Auto)
            .unwrap();
        assert_eq!(out.resolutions[0].tier_used, 2);
        assert!(out.content.contains("let x = 2;"));
    }

    #[test]
    fn no_anchor_resolves_at_tier_2_via_context() {
        let original = "fn main() {\n    let x = 1;\n}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(out.resolutions[0].tier_used, 2);
        assert!(matches!(out.resolutions[0].confidence, Confidence::Medium));
        assert!(out.content.contains("let x = 2;"));
    }

    #[test]
    fn context_mismatch_in_entity_range_falls_through_to_tier_2() {
        let original = "\
fn alpha() {
    let a = 1;
}

fn beta() {
    let b = 1;
}
";
        // The anchor names `alpha`, but the hunk's context is `beta`'s body.
        // The extractor locates the `alpha` entity (lines 1-3), yet the
        // context cannot match inside it, so resolution falls through to the
        // full-file tier-2 search.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@ fn alpha
 fn beta() {
-    let b = 1;
+    let b = 2;
 }
";
        let file_path = Path::new("file.rs");
        let mock = MockExtractor {
            entities: vec![entity("alpha", 1, 3), entity("beta", 5, 7)],
        };
        let extractor: Option<&dyn EntityExtractor> = Some(&mock);
        let out = apply_unified_tiered(diff_text, original, file_path, extractor, PatchMode::Auto)
            .unwrap();
        assert_eq!(out.resolutions[0].tier_used, 2);
        assert!(out.content.contains("let b = 2;"));
        assert!(out.content.contains("let a = 1;"));
    }

    #[test]
    fn pure_insertion_resolves_at_tier_3_via_diffy() {
        let original = "fn main() {\n    let x = 1;\n}\n";
        // A zero-context insertion hunk: tier 1 (no anchor) and tier 2 (empty
        // pre-image) cannot place it, so diffy positions it by the corrected
        // `@@` header at tier 3.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -2,0 +2,1 @@
+    let y = 2;
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(out.lines_added, 1);
        assert_eq!(out.resolutions.len(), 1);
        assert_eq!(out.resolutions[0].tier_used, 3);
        assert!(matches!(out.resolutions[0].confidence, Confidence::Low));
        assert!(out.resolutions[0].matched_at_line.is_none());
        assert_eq!(out.resolutions[0].drift, 0);
        assert!(out.content.contains("let y = 2;"));
    }

    #[test]
    fn resolution_metadata_populated() {
        let original = "fn a() {\n    let x = 1;\n}\n\nfn b() {\n    let y = 1;\n}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn a() {
-    let x = 1;
+    let x = 2;
 }
@@ -5,3 +5,3 @@
 fn b() {
-    let y = 1;
+    let y = 2;
 }
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(out.resolutions.len(), out.hunks_applied);
        assert_eq!(out.resolutions.len(), 2);
        // First hunk: stated line 1, lands at line 1, no drift.
        assert_eq!(out.resolutions[0].stated_line, 1);
        assert_eq!(out.resolutions[0].matched_at_line, Some(1));
        assert_eq!(out.resolutions[0].drift, 0);
        assert_eq!(out.resolutions[0].tier_used, 2);
        // Second hunk: stated line 5, lands at line 5, no drift.
        assert_eq!(out.resolutions[1].stated_line, 5);
        assert_eq!(out.resolutions[1].matched_at_line, Some(5));
        assert_eq!(out.resolutions[1].drift, 0);
        assert_eq!(out.resolutions[1].tier_used, 2);
    }

    #[test]
    fn wrong_counts_resolve_via_context_at_tier_2() {
        let original = "fn main() {\n    let x = 1;\n}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,99 +1,99 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(out.hunks_applied, 1);
        assert_eq!(out.lines_added, 1);
        assert_eq!(out.lines_removed, 1);
        assert_eq!(out.resolutions[0].tier_used, 2);
        assert!(out.content.contains("let x = 2;"));
        assert!(!out.content.contains("let x = 1;"));
    }

    #[test]
    fn context_search_handles_line_drift() {
        let original = "// header comment\n// another comment\nfn main() {\n    let x = 1;\n}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(out.resolutions[0].tier_used, 2);
        assert!(out.content.contains("let x = 2;"));
        assert!(out.content.contains("// header comment"));
    }

    #[test]
    fn context_search_fuzzy_trailing_whitespace() {
        let original = "fn main() {  \n    let x = 1;\n}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        // Trailing-whitespace-only difference resolves via the trim strategy.
        assert_eq!(out.resolutions[0].tier_used, 2);
        assert!(matches!(out.resolutions[0].confidence, Confidence::Low));
        assert!(out.content.contains("let x = 2;"));
    }

    #[test]
    fn multi_hunk_application() {
        let original = "fn a() {\n    let x = 1;\n}\n\nfn b() {\n    let y = 1;\n}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn a() {
-    let x = 1;
+    let x = 2;
 }
@@ -5,3 +5,3 @@
 fn b() {
-    let y = 1;
+    let y = 2;
 }
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(out.hunks_applied, 2);
        assert_eq!(out.lines_added, 2);
        assert_eq!(out.lines_removed, 2);
        assert!(out.content.contains("let x = 2;"));
        assert!(out.content.contains("let y = 2;"));
    }

    #[test]
    fn context_not_found_gives_clear_error() {
        let original = "fn main() {}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn nonexistent() {
-    old_line;
+    new_line;
 }
";
        let file_path = Path::new("file.rs");
        let err = apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("context not found"), "got: {msg}");
    }

    #[test]
    fn preserves_trailing_newline() {
        let original = "line1\nline2\nline3\n";
        let diff_text = "\
--- a/file.txt
+++ b/file.txt
@@ -1,3 +1,3 @@
 line1
-line2
+line2_modified
 line3
";
        let file_path = Path::new("file.txt");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert!(out.content.ends_with('\n'));
        assert!(out.content.contains("line2_modified"));
        assert!(
            out.trailing_newline.is_none(),
            "no marker → no trailing-newline override"
        );
    }

    // --- H9 regression: occurrence disambiguation -------------------------

    /// A file with two byte-identical blocks. The historical bug applied a
    /// hunk targeting the *second* block at the first match and committed
    /// silently; the stated `@@` line must now disambiguate.
    fn duplicate_blocks_original() -> &'static str {
        "\
fn first() {
    let value = 1;
    process();
}

// spacer
fn second() {
    let value = 1;
    process();
}
"
    }

    #[test]
    fn duplicate_blocks_patch_lands_at_stated_line() {
        // Hunk context (the shared body) matches at lines 2 and 8. The header
        // states line 8, so the second occurrence must be patched.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -8,2 +8,2 @@
-    let value = 1;
+    let value = 2;
     process();
";
        let out = apply_unified_tiered(
            diff_text,
            duplicate_blocks_original(),
            Path::new("file.rs"),
            None,
            PatchMode::Auto,
        )
        .unwrap();
        assert_eq!(out.hunks_applied, 1);
        assert_eq!(out.resolutions[0].matched_at_line, Some(8));
        assert_eq!(out.resolutions[0].drift, 0);
        let lines: Vec<&str> = out.content.lines().collect();
        assert_eq!(lines[1], "    let value = 1;", "first block untouched");
        assert_eq!(lines[7], "    let value = 2;", "second block patched");
    }

    #[test]
    fn duplicate_blocks_patch_lands_at_first_when_stated() {
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -2,2 +2,2 @@
-    let value = 1;
+    let value = 2;
     process();
";
        let out = apply_unified_tiered(
            diff_text,
            duplicate_blocks_original(),
            Path::new("file.rs"),
            None,
            PatchMode::Auto,
        )
        .unwrap();
        assert_eq!(out.resolutions[0].matched_at_line, Some(2));
        let lines: Vec<&str> = out.content.lines().collect();
        assert_eq!(lines[1], "    let value = 2;", "first block patched");
        assert_eq!(lines[7], "    let value = 1;", "second block untouched");
    }

    #[test]
    fn duplicate_blocks_with_equidistant_stated_line_refuse() {
        // Matches at lines 2 and 8 (0-based 1 and 7); stated line 5 (0-based
        // 4) is equally distant from both and cannot disambiguate — the patch
        // must refuse with the candidate locations, never guess.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -5,2 +5,2 @@
-    let value = 1;
+    let value = 2;
     process();
";
        let err = apply_unified_tiered(
            diff_text,
            duplicate_blocks_original(),
            Path::new("file.rs"),
            None,
            PatchMode::Auto,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("multiple locations"), "got: {msg}");
        assert!(msg.contains("lines 2, 8"), "candidates listed: {msg}");
        assert!(msg.contains("refusing"), "got: {msg}");
    }

    // --- NTP-005 R3: strict mode -----------------------------------------

    #[test]
    fn strict_applies_at_exact_stated_line() {
        let original = "fn main() {\n    let x = 1;\n}\n";
        // Context matches byte-for-byte at the stated line 1.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Strict).unwrap();
        assert_eq!(out.hunks_applied, 1);
        assert_eq!(out.resolutions.len(), 1);
        assert!(out.resolutions[0].applied);
        assert_eq!(out.resolutions[0].tier_used, 0);
        assert_eq!(out.resolutions[0].matched_at_line, Some(1));
        assert_eq!(out.resolutions[0].drift, 0);
        assert!(out.resolutions[0].failure.is_none());
        assert!(out.resolutions[0].structural_alternative.is_none());
        assert!(out.content.contains("let x = 2;"));
        assert!(!out.content.contains("let x = 1;"));
    }

    #[test]
    fn strict_fails_on_drift_and_reports_structural_alternative() {
        // The target body is at lines 2-4, but the hunk states line 1.
        let original = "// leading comment\nfn main() {\n    let x = 1;\n}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Strict).unwrap();
        // Nothing applied: the whole patch is not aborted, but the hunk is
        // left unplaced and the totals reflect that.
        assert_eq!(out.hunks_applied, 0);
        assert_eq!(out.lines_added, 0);
        assert_eq!(out.lines_removed, 0);
        assert_eq!(out.content, original, "buffer unchanged on strict failure");
        assert_eq!(out.resolutions.len(), 1);
        assert!(!out.resolutions[0].applied);
        assert_eq!(out.resolutions[0].tier_used, 0);
        assert!(out.resolutions[0].failure.is_some());

        let alt = out.resolutions[0]
            .structural_alternative
            .as_ref()
            .expect("structural alternative present");
        assert!(alt.would_apply);
        // Context lives at line 2 but the header said line 1 → drift +1.
        assert_eq!(alt.matched_at_line, Some(2));
        assert_eq!(alt.drift, 1);
        // No extractor → context-search alternative, no entity name.
        assert!(alt.entity.is_none());
    }

    #[test]
    fn strict_failure_with_no_match_has_null_alternative() {
        let original = "fn main() {}\n";
        // Stated line 5 is past EOF and the context exists nowhere.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -5,3 +5,3 @@
 fn nonexistent() {
-    old_line;
+    new_line;
 }
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Strict).unwrap();
        assert_eq!(out.hunks_applied, 0);
        assert_eq!(out.content, original);
        assert_eq!(out.resolutions.len(), 1);
        assert!(!out.resolutions[0].applied);
        assert_eq!(out.resolutions[0].tier_used, 0);
        assert!(
            out.resolutions[0].structural_alternative.is_none(),
            "no viable structural match → null alternative",
        );
    }

    #[test]
    fn strict_failure_entity_alternative_names_entity() {
        let original = "\
fn other() {
    let a = 0;
}

fn target() {
    let value = 1;
}
";
        // Stated line 1 is wrong (target is at lines 5-7) and the context does
        // not match there, so strict fails. With an extractor the structural
        // probe finds `target` and reports the entity.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@ fn target
 fn target() {
-    let value = 1;
+    let value = 2;
 }
";
        let file_path = Path::new("file.rs");
        let mock = MockExtractor {
            entities: vec![entity("other", 1, 3), entity("target", 5, 7)],
        };
        let extractor: Option<&dyn EntityExtractor> = Some(&mock);
        let out =
            apply_unified_tiered(diff_text, original, file_path, extractor, PatchMode::Strict)
                .unwrap();
        assert_eq!(out.hunks_applied, 0);
        assert!(!out.resolutions[0].applied);
        let alt = out.resolutions[0]
            .structural_alternative
            .as_ref()
            .expect("entity-guided structural alternative present");
        assert!(alt.would_apply);
        assert_eq!(alt.entity.as_deref(), Some("fn target"));
        assert_eq!(alt.entity_range, Some((5, 7)));
        assert_eq!(alt.matched_at_line, Some(5));
        assert_eq!(alt.drift, 4);
        assert!(matches!(alt.confidence, Confidence::High));
    }

    #[test]
    fn strict_continues_past_a_failed_hunk() {
        // First hunk drifted (fails strict), second hunk at exact position
        // (applies). The patch is not aborted by the first failure.
        let original = "// comment\nfn a() {\n    let x = 1;\n}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn a() {
-    let x = 1;
+    let x = 2;
@@ -1,1 +1,1 @@
-// comment
+// banner
";
        let file_path = Path::new("file.rs");
        let out =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Strict).unwrap();
        assert_eq!(out.resolutions.len(), 2);
        assert!(!out.resolutions[0].applied, "drifted hunk fails strict");
        assert!(out.resolutions[1].applied, "exact-position hunk applies");
        assert_eq!(out.hunks_applied, 1);
        assert!(out.content.contains("// banner"));
        assert!(
            out.content.contains("let x = 1;"),
            "first hunk left unapplied"
        );
    }

    #[test]
    fn strict_multi_hunk_offsets_later_hunks_by_applied_delta() {
        // Hunk 1 grows the file by one line; hunk 2's @@ header is stated
        // against the ORIGINAL numbering (line 5) and must be translated by
        // the net delta of already-applied hunks to land at line 6.
        let original = "fn a() {\n    let x = 1;\n}\nfn b() {\n    let y = 1;\n}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -2,1 +2,2 @@
-    let x = 1;
+    let x = 2;
+    let x_extra = 3;
@@ -5,1 +5,1 @@
-    let y = 1;
+    let y = 2;
";
        let out = apply_unified_tiered(
            diff_text,
            original,
            Path::new("file.rs"),
            None,
            PatchMode::Strict,
        )
        .unwrap();
        assert_eq!(
            out.hunks_applied, 2,
            "both hunks must apply strictly: {:?}",
            out.resolutions
        );
        assert!(out.resolutions[0].applied);
        assert!(out.resolutions[1].applied, "{:?}", out.resolutions[1]);
        assert_eq!(out.resolutions[1].matched_at_line, Some(6));
        assert_eq!(out.resolutions[1].drift, 1);
        assert!(out.content.contains("let x_extra = 3;"));
        assert!(out.content.contains("let y = 2;"));
    }

    #[test]
    fn strict_multi_hunk_offsets_after_removal() {
        // Hunk 1 shrinks the file by one line; hunk 2 states line 4 (original
        // numbering) but its context now lives at line 3.
        let original = "// banner\n// extra\nfn a() {\n    let x = 1;\n}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -2,1 +2,0 @@
-// extra
@@ -4,1 +4,1 @@
-    let x = 1;
+    let x = 2;
";
        let out = apply_unified_tiered(
            diff_text,
            original,
            Path::new("file.rs"),
            None,
            PatchMode::Strict,
        )
        .unwrap();
        assert_eq!(
            out.hunks_applied, 2,
            "both hunks must apply strictly: {:?}",
            out.resolutions
        );
        assert!(out.resolutions[1].applied, "{:?}", out.resolutions[1]);
        assert_eq!(out.resolutions[1].matched_at_line, Some(3));
        assert_eq!(out.resolutions[1].drift, -1);
        assert!(!out.content.contains("// extra"));
        assert!(out.content.contains("let x = 2;"));
    }

    // --- `\ No newline at end of file` marker -----------------------------

    #[test]
    fn no_newline_marker_is_not_treated_as_context() {
        // A real git-format diff against a file lacking a trailing newline
        // carries the marker after both the old and new final lines. The
        // marker must be consumed by the parser, not matched as context.
        let original = "alpha\nbeta";
        let diff_text = "\
--- a/f.txt
+++ b/f.txt
@@ -1,2 +1,2 @@
 alpha
-beta
\\ No newline at end of file
+BETA
\\ No newline at end of file
";
        let out = apply_unified_tiered(
            diff_text,
            original,
            Path::new("f.txt"),
            None,
            PatchMode::Auto,
        )
        .unwrap();
        assert_eq!(out.hunks_applied, 1);
        assert!(out.content.contains("BETA"));
        assert!(!out.content.contains("No newline"));
        assert_eq!(
            out.trailing_newline,
            Some(false),
            "new side declared no trailing newline"
        );
    }

    #[test]
    fn no_newline_marker_after_context_line_parses() {
        // Change earlier in the file; the trailing context line carries the
        // marker (the file lacks a trailing newline on both sides).
        let original = "alpha\nbeta\nomega";
        let diff_text = "\
--- a/f.txt
+++ b/f.txt
@@ -1,3 +1,3 @@
 alpha
-beta
+BETA
 omega
\\ No newline at end of file
";
        let out = apply_unified_tiered(
            diff_text,
            original,
            Path::new("f.txt"),
            None,
            PatchMode::Auto,
        )
        .unwrap();
        assert_eq!(out.hunks_applied, 1);
        assert!(out.content.contains("BETA"));
        assert!(out.content.contains("omega"));
        assert!(!out.content.contains("No newline"));
        assert_eq!(out.trailing_newline, Some(false));
    }

    #[test]
    fn old_side_marker_alone_requests_trailing_newline() {
        // The old final line lacks a newline; the new side carries no marker,
        // so the patched file must gain a trailing newline.
        let original = "alpha\nbeta";
        let diff_text = "\
--- a/f.txt
+++ b/f.txt
@@ -2,1 +2,2 @@
-beta
\\ No newline at end of file
+beta
+gamma
";
        let out = apply_unified_tiered(
            diff_text,
            original,
            Path::new("f.txt"),
            None,
            PatchMode::Auto,
        )
        .unwrap();
        assert_eq!(out.hunks_applied, 1);
        assert!(out.content.contains("gamma"));
        assert_eq!(out.trailing_newline, Some(true));
    }

    #[test]
    fn unapplied_strict_hunk_does_not_set_trailing_newline_hint() {
        // The marker-carrying hunk fails strict matching (wrong stated line),
        // so it must not change the trailing-newline state.
        let original = "alpha\nbeta\n";
        let diff_text = "\
--- a/f.txt
+++ b/f.txt
@@ -7,1 +7,1 @@
-beta
+BETA
\\ No newline at end of file
";
        let out = apply_unified_tiered(
            diff_text,
            original,
            Path::new("f.txt"),
            None,
            PatchMode::Strict,
        )
        .unwrap();
        assert_eq!(out.hunks_applied, 0);
        assert!(
            out.trailing_newline.is_none(),
            "unapplied hunk's marker must not leak into the hint"
        );
    }

    // --- NTP-005 R5: structural mode -------------------------------------

    #[test]
    fn structural_succeeds_with_valid_anchor_and_entity() {
        let original = "\
fn other() {
    let a = 0;
}

fn target() {
    let value = 1;
}
";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -10,3 +10,3 @@ fn target
 fn target() {
-    let value = 1;
+    let value = 2;
 }
";
        let file_path = Path::new("file.rs");
        let mock = MockExtractor {
            entities: vec![entity("other", 1, 3), entity("target", 5, 7)],
        };
        let extractor: Option<&dyn EntityExtractor> = Some(&mock);
        let out = apply_unified_tiered(
            diff_text,
            original,
            file_path,
            extractor,
            PatchMode::Structural,
        )
        .unwrap();
        assert_eq!(out.hunks_applied, 1);
        assert!(out.resolutions[0].applied);
        assert_eq!(out.resolutions[0].tier_used, 1);
        assert_eq!(
            out.resolutions[0].entity_matched.as_deref(),
            Some("fn target")
        );
        assert_eq!(out.resolutions[0].matched_at_line, Some(5));
        assert!(matches!(out.resolutions[0].confidence, Confidence::High));
        assert!(out.content.contains("let value = 2;"));
    }

    #[test]
    fn structural_fails_without_semantic_anchor() {
        let original = "fn main() {\n    let x = 1;\n}\n";
        // No anchor after the second `@@`, even though context would match.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
";
        let file_path = Path::new("file.rs");
        let mock = MockExtractor {
            entities: vec![entity("main", 1, 3)],
        };
        let extractor: Option<&dyn EntityExtractor> = Some(&mock);
        let out = apply_unified_tiered(
            diff_text,
            original,
            file_path,
            extractor,
            PatchMode::Structural,
        )
        .unwrap();
        assert_eq!(out.hunks_applied, 0);
        assert_eq!(out.content, original);
        assert!(!out.resolutions[0].applied);
        assert_eq!(out.resolutions[0].tier_used, 0);
        let failure = out.resolutions[0]
            .failure
            .as_ref()
            .expect("failure reason present");
        assert!(
            failure.contains("semantic anchor"),
            "clear message: {failure}",
        );
        // Structural mode reports no structural_alternative (it is itself the
        // structural path).
        assert!(out.resolutions[0].structural_alternative.is_none());
    }

    #[test]
    fn structural_fails_when_entity_not_found() {
        let original = "fn main() {\n    let x = 1;\n}\n";
        // Anchor names an entity the extractor does not report.
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@ fn missing
 fn main() {
-    let x = 1;
+    let x = 2;
 }
";
        let file_path = Path::new("file.rs");
        let mock = MockExtractor {
            entities: vec![entity("main", 1, 3)],
        };
        let extractor: Option<&dyn EntityExtractor> = Some(&mock);
        let out = apply_unified_tiered(
            diff_text,
            original,
            file_path,
            extractor,
            PatchMode::Structural,
        )
        .unwrap();
        assert_eq!(out.hunks_applied, 0);
        assert_eq!(out.content, original);
        assert!(!out.resolutions[0].applied);
        let failure = out.resolutions[0].failure.as_ref().expect("failure reason");
        assert!(failure.contains("missing"), "names the entity: {failure}");
    }

    #[test]
    fn structural_does_not_fall_back_to_diffy() {
        // A pure insertion has no anchor and no context. Auto would place it
        // via tier-3 diffy; structural must refuse and fail non-fatally.
        let original = "fn main() {\n    let x = 1;\n}\n";
        let diff_text = "\
--- a/file.rs
+++ b/file.rs
@@ -2,0 +2,1 @@
+    let y = 2;
";
        let file_path = Path::new("file.rs");
        let mock = MockExtractor {
            entities: vec![entity("main", 1, 3)],
        };
        let extractor: Option<&dyn EntityExtractor> = Some(&mock);
        let out = apply_unified_tiered(
            diff_text,
            original,
            file_path,
            extractor,
            PatchMode::Structural,
        )
        .unwrap();
        assert_eq!(out.hunks_applied, 0, "no diffy fallback in structural mode");
        assert_eq!(out.content, original);
        assert!(!out.resolutions[0].applied);
    }
}
