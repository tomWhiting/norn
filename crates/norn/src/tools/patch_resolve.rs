//! Hunk placement machinery shared by every `apply_patch` resolution mode.
//!
//! Contains the per-hunk resolution records ([`HunkResolution`],
//! [`StructuralAlternative`]), the entity-anchor lookup, the hunk
//! application primitive, and the auto-mode tier resolver
//! ([`resolve_hunk`]): entity-guided placement → context-anchored search →
//! header-corrected diffy.

use std::path::Path;

use serde::Serialize;

use super::patch_entity::{EntityExtractor, ExtractedEntity};
use super::patch_hunk::{DiffLine, ParsedHunk};
use super::patch_match::{
    AmbiguousMatch, MatchKind, select_context_match, select_context_match_in_range,
};
use crate::tool::Confidence;

/// Per-hunk resolution detail recording which tier placed a hunk and how
/// confident the placement is. Surfaced through the tool output so callers
/// can report structural-vs-line resolution and calibrate accuracy.
#[derive(Clone, Debug, Serialize)]
pub(super) struct HunkResolution {
    /// Resolution tier: 1 (entity-guided), 2 (context search), 3 (diffy).
    /// `0` means the hunk was placed by exact stated-line matching (strict
    /// mode) or was not placed at all (a non-fatal strict/structural failure,
    /// in which case [`Self::applied`] is `false`).
    pub(super) tier_used: u8,
    /// Raw `@@` semantic anchor matched at tier 1 (e.g. `fn process_event`);
    /// `None` for tiers 2 and 3.
    pub(super) entity_matched: Option<String>,
    /// 1-based line where the hunk anchor landed; `None` when tier 3 leans on
    /// diffy's opaque placement or when the hunk did not apply.
    pub(super) matched_at_line: Option<usize>,
    /// 1-based `old_start` from the `@@` header.
    pub(super) stated_line: usize,
    /// `matched_at_line - stated_line` (0 when `matched_at_line` is `None`).
    pub(super) drift: i64,
    /// Confidence in the placement: tier 1 → High; tier 2 exact/whitespace →
    /// Medium, trim → Low; tier 3 → Low.
    pub(super) confidence: Confidence,
    /// Whether this hunk was applied to the file content. `true` for every
    /// auto-mode resolution (auto failures abort the whole patch) and for
    /// successful strict/structural placements; `false` for non-fatal
    /// strict/structural per-hunk failures.
    pub(super) applied: bool,
    /// Human-readable reason a strict/structural hunk did not apply; `None`
    /// when the hunk applied.
    pub(super) failure: Option<String>,
    /// For a failed strict-mode hunk, what structural matching (entity-guided
    /// or context search) would have found. `None` for applied hunks and for
    /// failed hunks with no viable structural placement.
    pub(super) structural_alternative: Option<StructuralAlternative>,
}

/// What structural resolution would have achieved for a hunk that failed
/// strict exact-position matching. Lets the model see the drift and entity a
/// structural retry (`mode: auto`) would land on, closing the accuracy
/// feedback loop.
#[derive(Clone, Debug, Serialize)]
pub(super) struct StructuralAlternative {
    /// Whether entity-guided or context-search resolution would have placed
    /// the hunk. Always `true` when this object is present (a non-viable
    /// alternative is reported as `null`).
    pub(super) would_apply: bool,
    /// Entity name from the hunk's `@@` anchor when entity-guided resolution
    /// matched; `None` when only full-file context search would have matched.
    pub(super) entity: Option<String>,
    /// 1-based inclusive line range of the matched entity, when entity-guided
    /// resolution matched.
    pub(super) entity_range: Option<(usize, usize)>,
    /// 1-based line where structural matching would have placed the hunk.
    pub(super) matched_at_line: Option<usize>,
    /// `matched_at_line - stated_line`: the signed offset from where the `@@`
    /// header claimed the hunk belonged.
    pub(super) drift: i64,
    /// Confidence in the structural placement.
    pub(super) confidence: Confidence,
}

/// Reconstruct a single-hunk unified diff block with corrected line counts and
/// synthetic file headers, suitable for feeding to `diffy` as the tier-3 last
/// resort. `offset` shifts the hunk's start by the net line delta of hunks
/// already applied to the working content, so diffy positions it near where it
/// now belongs (diffy still searches the whole file for an exact context
/// match).
fn reconstruct_single_hunk(path: &Path, hunk: &ParsedHunk, offset: i64) -> String {
    let display = path.display();
    let old_count = hunk.old_count();
    let new_count = hunk.new_count();
    let shifted_start = i64::try_from(hunk.old_start)
        .unwrap_or(1)
        .saturating_add(offset)
        .max(1);
    let shifted_start_u = u64::try_from(shifted_start).unwrap_or(1);
    let anchor_suffix = hunk
        .semantic_anchor
        .as_deref()
        .map_or(String::new(), |a| format!(" {a}"));
    // Built with infallible `String` pushes: writing into a `String` cannot
    // fail, and this avoids discarding a `fmt::Result` per line.
    let mut out = format!(
        "--- a/{display}\n+++ b/{display}\n@@ -{shifted_start_u},{old_count} \
         +{shifted_start_u},{new_count} @@{anchor_suffix}\n"
    );
    for dl in &hunk.lines {
        let (prefix, content) = match dl {
            DiffLine::Context(s) => (' ', s),
            DiffLine::Remove(s) => ('-', s),
            DiffLine::Add(s) => ('+', s),
        };
        out.push(prefix);
        out.push_str(content);
        out.push('\n');
    }
    out
}

/// Apply a single hunk at a known position by substituting old lines
/// with new lines. Returns the modified file content.
pub(super) fn apply_hunk_at(file_lines: &[&str], pos: usize, hunk: &ParsedHunk) -> String {
    let old_count = hunk.old_count();
    let new_lines = hunk.new_lines();
    let mut result: Vec<&str> = Vec::with_capacity(file_lines.len());
    result.extend_from_slice(&file_lines[..pos]);
    result.extend(new_lines.iter().copied());
    let after = pos + old_count;
    if after < file_lines.len() {
        result.extend_from_slice(&file_lines[after..]);
    }
    let mut out = result.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Locate the entity named by a hunk's `@@` semantic anchor via `extractor`,
/// returning its 0-based, inclusive line range `(start_row, end_row)`.
///
/// Matching mirrors the historical two-pass strategy, extended to consult each
/// entity's qualified name: first any entity whose `name` or `qualified_name`
/// is a substring of the anchor, then — if none matched — any whose `name` or
/// `qualified_name` equals the anchor's last whitespace-separated word. The
/// first match in each pass wins, so the result is stable across runs.
///
/// Returns `None` when the extractor does not support the file's language or
/// when no entity matches the anchor.
pub(super) fn entity_range_for_anchor(
    extractor: &dyn EntityExtractor,
    source: &str,
    path: &Path,
    anchor: &str,
) -> Option<(usize, usize)> {
    let entities = extractor.extract(source, path)?;

    let anchor_trimmed = anchor.trim();
    let anchor_last_word = anchor_trimmed
        .split_whitespace()
        .last()
        .unwrap_or(anchor_trimmed);

    for entity in &entities {
        if entity_name_contained(entity, anchor_trimmed) {
            return Some(entity_row_range(entity));
        }
    }

    for entity in &entities {
        if entity_name_equals(entity, anchor_last_word) {
            return Some(entity_row_range(entity));
        }
    }

    None
}

/// Whether `anchor` contains the entity's `name` or `qualified_name` as a
/// substring. Empty names never match (they would match every anchor).
fn entity_name_contained(entity: &ExtractedEntity, anchor: &str) -> bool {
    if let Some(name) = entity.name.as_deref()
        && !name.is_empty()
        && anchor.contains(name)
    {
        return true;
    }
    !entity.qualified_name.is_empty() && anchor.contains(entity.qualified_name.as_str())
}

/// Whether the entity's `name` or `qualified_name` equals `word` exactly.
fn entity_name_equals(entity: &ExtractedEntity, word: &str) -> bool {
    entity.name.as_deref() == Some(word) || entity.qualified_name == word
}

/// Convert an [`ExtractedEntity`]'s 1-indexed inclusive `line_range` into the
/// 0-based, inclusive `(start_row, end_row)` pair the tier-1 context search
/// expects. Saturating subtraction guards a degenerate `0` start.
fn entity_row_range(entity: &ExtractedEntity) -> (usize, usize) {
    let start = entity.line_range.start.saturating_sub(1);
    let end = entity.line_range.end.saturating_sub(1);
    (start, end)
}

/// Signed line drift between where a hunk landed (1-based) and where its `@@`
/// header said it would (1-based). Negative when the hunk landed earlier than
/// stated.
pub(super) fn line_drift(matched_at_line: usize, stated_line: usize) -> i64 {
    i64::try_from(matched_at_line).unwrap_or(0) - i64::try_from(stated_line).unwrap_or(0)
}

/// The 0-based position a hunk's `@@` header expects, adjusted by the net
/// line delta of hunks already applied. Used to prefer the nearest candidate
/// when a context window matches more than once.
pub(super) fn expected_position(stated_line: usize, cumulative_offset: i64) -> Option<usize> {
    let stated = i64::try_from(stated_line).ok()?;
    usize::try_from((stated - 1 + cumulative_offset).max(0)).ok()
}

/// Resolve a single hunk against `current` using entity-first tier ordering.
///
/// Returns `Ok(Some(..))` with the new file content and the
/// [`HunkResolution`] recording which tier placed the hunk, `Ok(None)` when
/// all three tiers fail (the caller turns that into a descriptive error),
/// and `Err` when a context search matched multiple windows that the stated
/// `@@` line cannot disambiguate — the caller must refuse rather than guess.
/// `file_lines` is `current` split into lines; `cumulative_offset` is the
/// net line delta of hunks already applied, used to adjust the expected
/// position and seed diffy's search in tier 3. `extractor` drives tier 1;
/// when `None`, tier 1 is skipped and resolution starts at tier 2.
pub(super) fn resolve_hunk(
    current: &str,
    file_lines: &[&str],
    hunk: &ParsedHunk,
    path: &Path,
    cumulative_offset: i64,
    extractor: Option<&dyn EntityExtractor>,
) -> Result<Option<(String, HunkResolution)>, AmbiguousMatch> {
    let old_lines = hunk.old_lines();
    let stated_line = hunk.old_start;
    let expected = expected_position(stated_line, cumulative_offset);

    // Tier 1: entity-guided placement — the strongest signal. The `@@`
    // semantic anchor names the target entity; scope the context search to
    // that entity's line range. Skipped when no extractor is supplied.
    // Ambiguity *within* the entity range is refused: the full-file search
    // would be at least as ambiguous.
    if let Some(extractor) = extractor
        && let Some(anchor) = hunk.semantic_anchor.as_deref()
        && let Some((entity_start, entity_end)) =
            entity_range_for_anchor(extractor, current, path, anchor)
        && let Some((pos, _)) = select_context_match_in_range(
            file_lines,
            &old_lines,
            entity_start,
            entity_end + 1,
            expected,
        )?
    {
        let matched = pos + 1;
        return Ok(Some((
            apply_hunk_at(file_lines, pos, hunk),
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
        )));
    }

    // Tier 2: full-file context-anchored search (exact → whitespace → trim),
    // occurrence-aware: duplicate windows are disambiguated by the stated
    // `@@` line (nearest candidate) or refused.
    if let Some((pos, kind)) = select_context_match(file_lines, &old_lines, expected)? {
        let matched = pos + 1;
        let confidence = match kind {
            MatchKind::Exact | MatchKind::Whitespace => Confidence::Medium,
            MatchKind::Trim => Confidence::Low,
        };
        return Ok(Some((
            apply_hunk_at(file_lines, pos, hunk),
            HunkResolution {
                tier_used: 2,
                entity_matched: None,
                matched_at_line: Some(matched),
                stated_line,
                drift: line_drift(matched, stated_line),
                confidence,
                applied: true,
                failure: None,
                structural_alternative: None,
            },
        )));
    }

    // Tier 3: header-corrected single-hunk diffy (last resort). Places hunks
    // tiers 1-2 cannot match by context — e.g. zero-context insertions — by
    // the corrected `@@` header. diffy applies only on an exact context match,
    // so it never silently misplaces a drifted hunk.
    let single = reconstruct_single_hunk(path, hunk, cumulative_offset);
    if let Ok(parsed) = diffy::Patch::from_str(&single)
        && let Ok(applied) = diffy::apply(current, &parsed)
    {
        return Ok(Some((
            applied,
            HunkResolution {
                tier_used: 3,
                entity_matched: None,
                matched_at_line: None,
                stated_line,
                drift: 0,
                confidence: Confidence::Low,
                applied: true,
                failure: None,
                structural_alternative: None,
            },
        )));
    }

    Ok(None)
}

/// Non-mutating probe: would entity-guided or full-file context resolution
/// place this hunk? Returns the structural placement a strict-mode retry
/// (`mode: auto`) would land on, or `None` when no viable structural match
/// exists. Mirrors the tier-1/tier-2 search in [`resolve_hunk`] without
/// touching the working buffer. `cumulative_offset` adjusts the expected
/// position by the net line delta of hunks already applied, exactly as the
/// real resolver would see it.
pub(super) fn probe_structural_alternative(
    current: &str,
    file_lines: &[&str],
    hunk: &ParsedHunk,
    path: &Path,
    cumulative_offset: i64,
    extractor: Option<&dyn EntityExtractor>,
) -> Option<StructuralAlternative> {
    let old_lines = hunk.old_lines();
    let stated_line = hunk.old_start;
    let expected = expected_position(stated_line, cumulative_offset);

    // Entity-guided placement (the structural signal): scope the context
    // search to the entity named by the `@@` anchor. An ambiguous match is
    // not a viable alternative (a structural retry would refuse it too).
    if let Some(extractor) = extractor
        && let Some(anchor) = hunk.semantic_anchor.as_deref()
        && let Some((entity_start, entity_end)) =
            entity_range_for_anchor(extractor, current, path, anchor)
        && let Ok(Some((pos, _))) = select_context_match_in_range(
            file_lines,
            &old_lines,
            entity_start,
            entity_end + 1,
            expected,
        )
    {
        let matched = pos + 1;
        return Some(StructuralAlternative {
            would_apply: true,
            entity: Some(anchor.to_string()),
            entity_range: Some((entity_start + 1, entity_end + 1)),
            matched_at_line: Some(matched),
            drift: line_drift(matched, stated_line),
            confidence: Confidence::High,
        });
    }

    // Full-file context search (exact → whitespace → trim), occurrence-aware.
    if let Ok(Some((pos, kind))) = select_context_match(file_lines, &old_lines, expected) {
        let matched = pos + 1;
        let confidence = match kind {
            MatchKind::Exact | MatchKind::Whitespace => Confidence::Medium,
            MatchKind::Trim => Confidence::Low,
        };
        return Some(StructuralAlternative {
            would_apply: true,
            entity: None,
            entity_range: None,
            matched_at_line: Some(matched),
            drift: line_drift(matched, stated_line),
            confidence,
        });
    }

    None
}
