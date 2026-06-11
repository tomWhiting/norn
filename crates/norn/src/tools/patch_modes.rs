//! Strict and structural hunk resolution for `apply_patch`.
//!
//! * **Strict** — apply a hunk only when its old lines match byte-for-byte at
//!   the stated `@@` position (translated by the net line delta of hunks
//!   already applied). No tier fallback; failures are non-fatal and carry a
//!   structural-alternative probe.
//! * **Structural** — require a semantic anchor and successful entity
//!   resolution for every hunk; context search is scoped to the entity's
//!   range and never falls back to diffy.

use std::path::Path;

use super::patch_entity::EntityExtractor;
use super::patch_hunk::ParsedHunk;
use super::patch_match::select_context_match_in_range;
use super::patch_resolve::{
    HunkResolution, apply_hunk_at, entity_range_for_anchor, expected_position, line_drift,
    probe_structural_alternative,
};
use crate::tool::Confidence;

/// The stated `@@` line translated by the net line delta of hunks already
/// applied, so a multi-hunk strict patch keeps matching after an earlier hunk
/// grows or shrinks the file. Clamped to `min` (1 for context-bearing hunks,
/// whose position is 1-based; 0 for pure insertions, whose stated line is
/// used directly as a 0-based insertion point).
fn translated_stated_line(stated_line: usize, cumulative_offset: i64, min: i64) -> usize {
    let translated = i64::try_from(stated_line)
        .unwrap_or(i64::MAX)
        .saturating_add(cumulative_offset)
        .max(min);
    usize::try_from(translated).unwrap_or(stated_line)
}

/// Strict-mode resolution: apply a hunk only when its old lines match
/// byte-for-byte at the exact stated `@@` position, translated by
/// `cumulative_offset` (the net line delta of hunks already applied — the
/// `@@` headers of a multi-hunk patch are stated against the *original*
/// file). No tier fallback. On failure the hunk is left unapplied
/// (non-fatal) and a structural-alternative probe records what a
/// `mode: auto` retry would have found.
///
/// Returns the new file content when applied (`None` leaves the buffer
/// unchanged) alongside the [`HunkResolution`].
pub(super) fn resolve_hunk_strict(
    current: &str,
    file_lines: &[&str],
    hunk: &ParsedHunk,
    path: &Path,
    cumulative_offset: i64,
    extractor: Option<&dyn EntityExtractor>,
) -> (Option<String>, HunkResolution) {
    let stated_line = hunk.old_start;
    let old_lines = hunk.old_lines();
    let old_count = hunk.old_count();

    // 0-based position the `@@` header claims, adjusted for hunks already
    // applied. A pure insertion (no old lines) carries no context to verify,
    // so strict trusts the (translated) stated position directly; otherwise
    // the old lines must match exactly at that position.
    let pos = if old_count == 0 {
        translated_stated_line(stated_line, cumulative_offset, 0).min(file_lines.len())
    } else {
        translated_stated_line(stated_line, cumulative_offset, 1) - 1
    };

    let matches_exactly = if old_count == 0 {
        true
    } else {
        pos + old_count <= file_lines.len()
            && file_lines[pos..pos + old_count]
                .iter()
                .zip(old_lines.iter())
                .all(|(a, b)| a == b)
    };

    if matches_exactly {
        let matched = pos + 1;
        return (
            Some(apply_hunk_at(file_lines, pos, hunk)),
            HunkResolution {
                tier_used: 0,
                entity_matched: None,
                matched_at_line: Some(matched),
                stated_line,
                drift: line_drift(matched, stated_line),
                confidence: Confidence::High,
                applied: true,
                failure: None,
                structural_alternative: None,
            },
        );
    }

    let structural_alternative = probe_structural_alternative(
        current,
        file_lines,
        hunk,
        path,
        cumulative_offset,
        extractor,
    );
    (
        None,
        HunkResolution {
            tier_used: 0,
            entity_matched: None,
            matched_at_line: None,
            stated_line,
            drift: 0,
            confidence: Confidence::Low,
            applied: false,
            failure: Some(format!(
                "strict mode: context did not match exactly at stated line {stated_line}"
            )),
            structural_alternative,
        },
    )
}

/// Structural-mode resolution: require a semantic anchor and successful entity
/// resolution for every hunk, search context only within the entity's scoped
/// range, and never fall back to diffy. On failure the hunk is left unapplied
/// (non-fatal) with a clear per-hunk reason. `cumulative_offset` adjusts the
/// expected position used to disambiguate duplicate context windows.
///
/// Returns the new file content when applied (`None` leaves the buffer
/// unchanged) alongside the [`HunkResolution`].
pub(super) fn resolve_hunk_structural(
    current: &str,
    file_lines: &[&str],
    hunk: &ParsedHunk,
    path: &Path,
    cumulative_offset: i64,
    extractor: Option<&dyn EntityExtractor>,
) -> (Option<String>, HunkResolution) {
    let stated_line = hunk.old_start;
    let old_lines = hunk.old_lines();

    let fail = |reason: String| {
        (
            None,
            HunkResolution {
                tier_used: 0,
                entity_matched: None,
                matched_at_line: None,
                stated_line,
                drift: 0,
                confidence: Confidence::Low,
                applied: false,
                failure: Some(reason),
                structural_alternative: None,
            },
        )
    };

    let Some(extractor) = extractor else {
        return fail(
            "structural mode: no entity extractor is configured, so entity resolution is impossible"
                .to_string(),
        );
    };
    let Some(anchor) = hunk.semantic_anchor.as_deref() else {
        return fail(
            "structural mode: hunk has no semantic anchor (@@ ... @@ <entity>) to resolve against"
                .to_string(),
        );
    };
    let Some((entity_start, entity_end)) =
        entity_range_for_anchor(extractor, current, path, anchor)
    else {
        return fail(format!(
            "structural mode: entity '{anchor}' was not found by the entity extractor"
        ));
    };
    let selected = match select_context_match_in_range(
        file_lines,
        &old_lines,
        entity_start,
        entity_end + 1,
        expected_position(stated_line, cumulative_offset),
    ) {
        Ok(found) => found,
        Err(ambiguous) => {
            return fail(format!(
                "structural mode: hunk context matches multiple locations within entity \
                 '{anchor}' ({}) and the stated line does not disambiguate them",
                ambiguous.describe_candidates(),
            ));
        }
    };
    let Some((pos, _)) = selected else {
        return fail(format!(
            "structural mode: hunk context did not match within entity '{anchor}' (lines {}-{})",
            entity_start + 1,
            entity_end + 1,
        ));
    };

    let matched = pos + 1;
    (
        Some(apply_hunk_at(file_lines, pos, hunk)),
        HunkResolution {
            tier_used: 1,
            entity_matched: Some(anchor.to_string()),
            matched_at_line: Some(matched),
            stated_line,
            drift: line_drift(matched, stated_line),
            confidence: Confidence::High,
            applied: true,
            failure: None,
            structural_alternative: None,
        },
    )
}
