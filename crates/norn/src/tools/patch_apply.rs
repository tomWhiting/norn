//! Three-tier unified diff application in entity-first order: entity-guided
//! placement → context-anchored search → header-corrected diffy.
//!
//! Models frequently generate unified diffs with stale line numbers in the
//! `@@ -a,b +c,d @@` headers, but the `@@` semantic anchor (the text after
//! the second `@@`, e.g. `fn process_event`) and the hunk's context lines
//! are almost always correct. Rather than trusting line numbers first, this
//! module resolves each hunk through progressively weaker signals:
//!
//! 1. **Entity-guided placement** — read the `@@` semantic anchor, use the
//!    injected [`EntityExtractor`] to find the named entity, and search for
//!    the hunk's context lines within that entity's line range. Structural
//!    identity is the strongest signal: it survives line drift and
//!    surrounding edits. Skipped entirely when no extractor is supplied.
//! 2. **Context-anchored search** — when there is no anchor or the entity
//!    is not found, search the whole file for the hunk's context lines
//!    (exact, then whitespace-insensitive, then trim-insensitive).
//! 3. **Header-corrected diffy** — last resort: reconstruct a single-hunk
//!    unified diff with corrected counts and apply it via `diffy`, which
//!    positions the hunk by an exact context match anywhere in the file.
//!
//! Each hunk's resolution is recorded in a [`HunkResolution`] so callers can
//! report which tier placed it, the entity matched, and the line drift.

use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;

use super::patch::PatchMode;
use super::patch_entity::{EntityExtractor, ExtractedEntity};
use crate::error::ToolError;
use crate::tool::Confidence;

/// A single line in a parsed hunk, preserving the interleaving order.
#[derive(Clone, Debug)]
enum DiffLine {
    Context(String),
    Remove(String),
    Add(String),
}

/// A parsed hunk from a unified diff with raw line ordering preserved.
#[derive(Clone, Debug)]
struct ParsedHunk {
    old_start: usize,
    semantic_anchor: Option<String>,
    lines: Vec<DiffLine>,
}

impl ParsedHunk {
    fn old_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                DiffLine::Context(s) | DiffLine::Remove(s) => Some(s.as_str()),
                DiffLine::Add(_) => None,
            })
            .collect()
    }

    fn new_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                DiffLine::Context(s) | DiffLine::Add(s) => Some(s.as_str()),
                DiffLine::Remove(_) => None,
            })
            .collect()
    }

    fn old_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Context(_) | DiffLine::Remove(_)))
            .count()
    }

    fn new_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Context(_) | DiffLine::Add(_)))
            .count()
    }

    fn add_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Add(_)))
            .count()
    }

    fn remove_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Remove(_)))
            .count()
    }
}

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

/// Extract the semantic anchor — the text after the second `@@` in a unified
/// diff hunk header. For `@@ -10,5 +10,6 @@ fn process_event` this returns
/// `Some("fn process_event")`. Returns `None` when there is no second `@@` or
/// the text after it is empty or whitespace-only.
///
/// Uses a forward scan (first `@@`, then the next `@@` in the remainder)
/// rather than `rfind`, so an anchor that itself contains `@@` is handled.
fn parse_semantic_anchor(header: &str) -> Option<String> {
    let first = header.find("@@")?;
    let rest = header.get(first + 2..)?;
    let off = rest.find("@@")?;
    let after = rest.get(off + 2..)?;
    let trimmed = after.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Parse the `@@` header line to extract the 1-based old start line and the
/// semantic anchor.
fn parse_hunk_header(line: &str) -> Result<(usize, Option<String>), String> {
    let stripped = line.trim_start_matches('@').trim_start();
    let old_part = stripped
        .strip_prefix('-')
        .ok_or_else(|| format!("malformed hunk header: {line}"))?;
    let start_str = old_part.split([',', ' ']).next().unwrap_or("1");
    let old_start = start_str
        .parse::<usize>()
        .map_err(|e| format!("bad start line in hunk header: {e}"))?;

    Ok((old_start, parse_semantic_anchor(line)))
}

/// Parse a single-file unified diff block into a sequence of hunks.
fn parse_unified_hunks(block: &str) -> Result<Vec<ParsedHunk>, String> {
    let mut hunks = Vec::new();
    let mut current: Option<ParsedHunk> = None;

    for line in block.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
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
            });
            continue;
        }
        if let Some(ref mut hunk) = current {
            if let Some(content) = line.strip_prefix('+') {
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

/// Reconstruct a single-hunk unified diff block with corrected line counts and
/// synthetic file headers, suitable for feeding to `diffy` as the tier-3 last
/// resort. `offset` shifts the hunk's start by the net line delta of hunks
/// already applied to the working content, so diffy positions it near where it
/// now belongs (diffy still searches the whole file for an exact context
/// match).
fn reconstruct_single_hunk(path: &Path, hunk: &ParsedHunk, offset: i64) -> String {
    let mut out = String::new();
    let display = path.display();
    let _ = writeln!(out, "--- a/{display}");
    let _ = writeln!(out, "+++ b/{display}");

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
    let _ = writeln!(
        out,
        "@@ -{shifted_start_u},{old_count} +{shifted_start_u},{new_count} @@{anchor_suffix}"
    );
    for dl in &hunk.lines {
        match dl {
            DiffLine::Context(s) => {
                let _ = writeln!(out, " {s}");
            }
            DiffLine::Remove(s) => {
                let _ = writeln!(out, "-{s}");
            }
            DiffLine::Add(s) => {
                let _ = writeln!(out, "+{s}");
            }
        }
    }
    out
}

/// Which matching strategy located a context window. Drives the confidence
/// recorded for a tier-2 resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchKind {
    /// Byte-identical match.
    Exact,
    /// Match after collapsing every run of whitespace to a single space.
    Whitespace,
    /// Match after trimming each line.
    Trim,
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

/// Search `file_lines` for a contiguous match of `pattern_lines`.
///
/// Tries three strategies in order: exact byte-equality, whitespace-
/// insensitive (runs of whitespace collapsed to a single space), then
/// trim-insensitive (each line trimmed). Returns the 0-based index of the
/// first match together with the strategy that found it, or `None`.
fn find_context_match(file_lines: &[&str], pattern_lines: &[&str]) -> Option<(usize, MatchKind)> {
    if pattern_lines.is_empty() || pattern_lines.len() > file_lines.len() {
        return None;
    }
    let window_len = pattern_lines.len();

    for i in 0..=file_lines.len() - window_len {
        if file_lines[i..i + window_len]
            .iter()
            .zip(pattern_lines.iter())
            .all(|(a, b)| *a == *b)
        {
            return Some((i, MatchKind::Exact));
        }
    }

    for i in 0..=file_lines.len() - window_len {
        if file_lines[i..i + window_len]
            .iter()
            .zip(pattern_lines.iter())
            .all(|(a, b)| collapse_ws(a) == collapse_ws(b))
        {
            return Some((i, MatchKind::Whitespace));
        }
    }

    for i in 0..=file_lines.len() - window_len {
        if file_lines[i..i + window_len]
            .iter()
            .zip(pattern_lines.iter())
            .all(|(a, b)| a.trim() == b.trim())
        {
            return Some((i, MatchKind::Trim));
        }
    }

    None
}

/// Search for a context match within a restricted line range, returning the
/// match position relative to the full file together with the strategy used.
fn find_context_in_range(
    file_lines: &[&str],
    pattern_lines: &[&str],
    range_start: usize,
    range_end: usize,
) -> Option<(usize, MatchKind)> {
    if pattern_lines.is_empty() || range_end <= range_start {
        return None;
    }
    let end = range_end.min(file_lines.len());
    let start = range_start.min(end);
    let slice = &file_lines[start..end];
    find_context_match(slice, pattern_lines).map(|(pos, kind)| (pos + start, kind))
}

/// Apply a single hunk at a known position by substituting old lines
/// with new lines. Returns the modified file content.
fn apply_hunk_at(file_lines: &[&str], pos: usize, hunk: &ParsedHunk) -> String {
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
fn entity_range_for_anchor(
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
fn line_drift(matched_at_line: usize, stated_line: usize) -> i64 {
    i64::try_from(matched_at_line).unwrap_or(0) - i64::try_from(stated_line).unwrap_or(0)
}

/// Resolve a single hunk against `current` using entity-first tier ordering.
///
/// Returns the new file content and the [`HunkResolution`] recording which tier
/// placed the hunk, or `None` when all three tiers fail (the caller turns that
/// into a descriptive error). `file_lines` is `current` split into lines;
/// `cumulative_offset` is the net line delta of hunks already applied, used to
/// seed diffy's search in tier 3. `extractor` drives tier 1; when `None`, tier
/// 1 is skipped and resolution starts at tier 2.
fn resolve_hunk(
    current: &str,
    file_lines: &[&str],
    hunk: &ParsedHunk,
    path: &Path,
    cumulative_offset: i64,
    extractor: Option<&dyn EntityExtractor>,
) -> Option<(String, HunkResolution)> {
    let old_lines = hunk.old_lines();
    let stated_line = hunk.old_start;

    // Tier 1: entity-guided placement — the strongest signal. The `@@`
    // semantic anchor names the target entity; scope the context search to
    // that entity's line range. Skipped when no extractor is supplied.
    if let Some(extractor) = extractor
        && let Some(anchor) = hunk.semantic_anchor.as_deref()
        && let Some((entity_start, entity_end)) =
            entity_range_for_anchor(extractor, current, path, anchor)
        && let Some((pos, _)) =
            find_context_in_range(file_lines, &old_lines, entity_start, entity_end + 1)
    {
        let matched = pos + 1;
        return Some((
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
        ));
    }

    // Tier 2: full-file context-anchored search (exact → whitespace → trim).
    if let Some((pos, kind)) = find_context_match(file_lines, &old_lines) {
        let matched = pos + 1;
        let confidence = match kind {
            MatchKind::Exact | MatchKind::Whitespace => Confidence::Medium,
            MatchKind::Trim => Confidence::Low,
        };
        return Some((
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
        ));
    }

    // Tier 3: header-corrected single-hunk diffy (last resort). Places hunks
    // tiers 1-2 cannot match by context — e.g. zero-context insertions — by
    // the corrected `@@` header. diffy applies only on an exact context match,
    // so it never silently misplaces a drifted hunk.
    let single = reconstruct_single_hunk(path, hunk, cumulative_offset);
    if let Ok(parsed) = diffy::Patch::from_str(&single)
        && let Ok(applied) = diffy::apply(current, &parsed)
    {
        return Some((
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
        ));
    }

    None
}

/// Non-mutating probe: would entity-guided or full-file context resolution
/// place this hunk? Returns the structural placement a strict-mode retry
/// (`mode: auto`) would land on, or `None` when no viable structural match
/// exists. Mirrors the tier-1/tier-2 search in [`resolve_hunk`] without
/// touching the working buffer.
fn probe_structural_alternative(
    current: &str,
    file_lines: &[&str],
    hunk: &ParsedHunk,
    path: &Path,
    extractor: Option<&dyn EntityExtractor>,
) -> Option<StructuralAlternative> {
    let old_lines = hunk.old_lines();
    let stated_line = hunk.old_start;

    // Entity-guided placement (the structural signal): scope the context
    // search to the entity named by the `@@` anchor.
    if let Some(extractor) = extractor
        && let Some(anchor) = hunk.semantic_anchor.as_deref()
        && let Some((entity_start, entity_end)) =
            entity_range_for_anchor(extractor, current, path, anchor)
        && let Some((pos, _)) =
            find_context_in_range(file_lines, &old_lines, entity_start, entity_end + 1)
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

    // Full-file context search (exact → whitespace → trim).
    if let Some((pos, kind)) = find_context_match(file_lines, &old_lines) {
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

/// Strict-mode resolution: apply a hunk only when its old lines match
/// byte-for-byte at the exact stated `@@` position. No tier fallback. On
/// failure the hunk is left unapplied (non-fatal) and a structural-alternative
/// probe records what a `mode: auto` retry would have found.
///
/// Returns the new file content when applied (`None` leaves the buffer
/// unchanged) alongside the [`HunkResolution`].
fn resolve_hunk_strict(
    current: &str,
    file_lines: &[&str],
    hunk: &ParsedHunk,
    path: &Path,
    extractor: Option<&dyn EntityExtractor>,
) -> (Option<String>, HunkResolution) {
    let stated_line = hunk.old_start;
    let old_lines = hunk.old_lines();
    let old_count = hunk.old_count();

    // 0-based position the `@@` header claims. A pure insertion (no old lines)
    // carries no context to verify, so strict trusts the stated position
    // directly; otherwise the old lines must match exactly at that position.
    let pos = if old_count == 0 {
        stated_line.min(file_lines.len())
    } else {
        stated_line.saturating_sub(1)
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

    let structural_alternative =
        probe_structural_alternative(current, file_lines, hunk, path, extractor);
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
/// (non-fatal) with a clear per-hunk reason.
///
/// Returns the new file content when applied (`None` leaves the buffer
/// unchanged) alongside the [`HunkResolution`].
fn resolve_hunk_structural(
    current: &str,
    file_lines: &[&str],
    hunk: &ParsedHunk,
    path: &Path,
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
    let Some((pos, _)) =
        find_context_in_range(file_lines, &old_lines, entity_start, entity_end + 1)
    else {
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

/// Mode-aware application of a unified diff block to file content.
///
/// `mode` selects how each hunk is resolved:
///
/// * [`PatchMode::Auto`] — the entity-first tier order in [`resolve_hunk`]
///   (entity → context → diffy). A hunk that no tier can place aborts the whole
///   patch with [`ToolError::ExecutionFailed`], preserving the historical
///   all-or-nothing contract.
/// * [`PatchMode::Strict`] — exact stated-line matching only. A hunk that does
///   not match exactly is left unapplied and recorded as a non-fatal failure
///   carrying its [`StructuralAlternative`]; processing continues.
/// * [`PatchMode::Structural`] — entity resolution required per hunk, context
///   scoped to the entity, no diffy fallback. Failures are non-fatal as in
///   strict mode.
///
/// Returns `(new_content, hunks_applied, lines_added, lines_removed,
/// resolutions)`, where `resolutions` carries one [`HunkResolution`] per hunk
/// in order. `lines_added`/`lines_removed`/`hunks_applied` count only hunks
/// that actually applied, so strict/structural failures do not inflate the
/// totals. `extractor` drives entity resolution; passing `None` skips tier 1
/// in auto mode and forces every structural-mode hunk to fail.
pub(super) fn apply_unified_tiered(
    raw: &str,
    original: &str,
    path: &Path,
    extractor: Option<&dyn EntityExtractor>,
    mode: PatchMode,
) -> Result<(String, usize, usize, usize, Vec<HunkResolution>), ToolError> {
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

    for (i, hunk) in hunks.iter().enumerate() {
        // Resolve in a nested scope so the `file_lines` borrow of `current` is
        // released before `current` is reassigned below.
        let (applied_content, resolution) = {
            let file_lines: Vec<&str> = current.lines().collect();
            match mode {
                PatchMode::Auto => {
                    let Some((applied, resolution)) = resolve_hunk(
                        &current,
                        &file_lines,
                        hunk,
                        path,
                        cumulative_offset,
                        extractor,
                    ) else {
                        return Err(auto_resolution_error(i, hunk, &current, path, extractor));
                    };
                    (Some(applied), resolution)
                }
                PatchMode::Strict => {
                    resolve_hunk_strict(&current, &file_lines, hunk, path, extractor)
                }
                PatchMode::Structural => {
                    resolve_hunk_structural(&current, &file_lines, hunk, path, extractor)
                }
            }
        };

        if let Some(applied) = applied_content {
            cumulative_offset += i64::try_from(hunk.new_count()).unwrap_or(0)
                - i64::try_from(hunk.old_count()).unwrap_or(0);
            current = applied;
            hunks_applied += 1;
            total_added += hunk.add_count();
            total_removed += hunk.remove_count();
        }
        resolutions.push(resolution);
    }

    Ok((
        current,
        hunks_applied,
        total_added,
        total_removed,
        resolutions,
    ))
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
    use super::*;

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
        let (result, _hunks, _added, _removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, extractor, PatchMode::Auto)
                .unwrap();
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].tier_used, 1);
        assert_eq!(resolutions[0].entity_matched.as_deref(), Some("fn target"));
        assert!(matches!(resolutions[0].confidence, Confidence::High));
        assert_eq!(resolutions[0].stated_line, 10);
        assert_eq!(resolutions[0].matched_at_line, Some(5));
        assert_eq!(resolutions[0].drift, -5);
        assert!(result.contains("let value = 2;"));
        assert!(!result.contains("let value = 1;"));
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
        let (result, _hunks, _added, _removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].tier_used, 2);
        assert!(result.contains("let value = 2;"));
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
        let (result, _hunks, _added, _removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, extractor, PatchMode::Auto)
                .unwrap();
        assert_eq!(resolutions[0].tier_used, 2);
        assert!(result.contains("let x = 2;"));
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
        let (result, _hunks, _added, _removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(resolutions[0].tier_used, 2);
        assert!(matches!(resolutions[0].confidence, Confidence::Medium));
        assert!(result.contains("let x = 2;"));
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
        let (result, _hunks, _added, _removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, extractor, PatchMode::Auto)
                .unwrap();
        assert_eq!(resolutions[0].tier_used, 2);
        assert!(result.contains("let b = 2;"));
        assert!(result.contains("let a = 1;"));
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
        let (result, _hunks, added, _removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(added, 1);
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].tier_used, 3);
        assert!(matches!(resolutions[0].confidence, Confidence::Low));
        assert!(resolutions[0].matched_at_line.is_none());
        assert_eq!(resolutions[0].drift, 0);
        assert!(result.contains("let y = 2;"));
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
        let (_result, hunks, _added, _removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(resolutions.len(), hunks);
        assert_eq!(resolutions.len(), 2);
        // First hunk: stated line 1, lands at line 1, no drift.
        assert_eq!(resolutions[0].stated_line, 1);
        assert_eq!(resolutions[0].matched_at_line, Some(1));
        assert_eq!(resolutions[0].drift, 0);
        assert_eq!(resolutions[0].tier_used, 2);
        // Second hunk: stated line 5, lands at line 5, no drift.
        assert_eq!(resolutions[1].stated_line, 5);
        assert_eq!(resolutions[1].matched_at_line, Some(5));
        assert_eq!(resolutions[1].drift, 0);
        assert_eq!(resolutions[1].tier_used, 2);
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
        let (result, hunks, added, removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(hunks, 1);
        assert_eq!(added, 1);
        assert_eq!(removed, 1);
        assert_eq!(resolutions[0].tier_used, 2);
        assert!(result.contains("let x = 2;"));
        assert!(!result.contains("let x = 1;"));
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
        let (result, _, _, _, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(resolutions[0].tier_used, 2);
        assert!(result.contains("let x = 2;"));
        assert!(result.contains("// header comment"));
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
        let (result, _, _, _, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        // Trailing-whitespace-only difference resolves via the trim strategy.
        assert_eq!(resolutions[0].tier_used, 2);
        assert!(matches!(resolutions[0].confidence, Confidence::Low));
        assert!(result.contains("let x = 2;"));
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
        let (result, hunks, added, removed, _resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert_eq!(hunks, 2);
        assert_eq!(added, 2);
        assert_eq!(removed, 2);
        assert!(result.contains("let x = 2;"));
        assert!(result.contains("let y = 2;"));
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
        let (result, _, _, _, _) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Auto).unwrap();
        assert!(result.ends_with('\n'));
        assert!(result.contains("line2_modified"));
    }

    #[test]
    fn find_context_match_exact() {
        let file: Vec<&str> = vec!["a", "b", "c", "d"];
        assert_eq!(
            find_context_match(&file, &["b", "c"]),
            Some((1, MatchKind::Exact))
        );
    }

    #[test]
    fn find_context_match_whitespace_collapse() {
        let file: Vec<&str> = vec!["fn  main()", "let  x = 1", "c"];
        assert_eq!(
            find_context_match(&file, &["fn main()", "let x = 1"]),
            Some((0, MatchKind::Whitespace))
        );
    }

    #[test]
    fn find_context_match_trimmed() {
        let file: Vec<&str> = vec!["a  ", "b  ", "c", "d"];
        assert_eq!(
            find_context_match(&file, &["a", "b"]),
            Some((0, MatchKind::Trim))
        );
    }

    #[test]
    fn find_context_match_not_found() {
        let file: Vec<&str> = vec!["a", "b", "c"];
        assert_eq!(find_context_match(&file, &["x", "y"]), None);
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
        let (result, hunks, _added, _removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Strict).unwrap();
        assert_eq!(hunks, 1);
        assert_eq!(resolutions.len(), 1);
        assert!(resolutions[0].applied);
        assert_eq!(resolutions[0].tier_used, 0);
        assert_eq!(resolutions[0].matched_at_line, Some(1));
        assert_eq!(resolutions[0].drift, 0);
        assert!(resolutions[0].failure.is_none());
        assert!(resolutions[0].structural_alternative.is_none());
        assert!(result.contains("let x = 2;"));
        assert!(!result.contains("let x = 1;"));
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
        let (result, hunks, added, removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Strict).unwrap();
        // Nothing applied: the whole patch is not aborted, but the hunk is
        // left unplaced and the totals reflect that.
        assert_eq!(hunks, 0);
        assert_eq!(added, 0);
        assert_eq!(removed, 0);
        assert_eq!(result, original, "buffer unchanged on strict failure");
        assert_eq!(resolutions.len(), 1);
        assert!(!resolutions[0].applied);
        assert_eq!(resolutions[0].tier_used, 0);
        assert!(resolutions[0].failure.is_some());

        let alt = resolutions[0]
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
        let (result, hunks, _added, _removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Strict).unwrap();
        assert_eq!(hunks, 0);
        assert_eq!(result, original);
        assert_eq!(resolutions.len(), 1);
        assert!(!resolutions[0].applied);
        assert_eq!(resolutions[0].tier_used, 0);
        assert!(
            resolutions[0].structural_alternative.is_none(),
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
        let (_result, hunks, _added, _removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, extractor, PatchMode::Strict)
                .unwrap();
        assert_eq!(hunks, 0);
        assert!(!resolutions[0].applied);
        let alt = resolutions[0]
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
        let (result, hunks, _added, _removed, resolutions) =
            apply_unified_tiered(diff_text, original, file_path, None, PatchMode::Strict).unwrap();
        assert_eq!(resolutions.len(), 2);
        assert!(!resolutions[0].applied, "drifted hunk fails strict");
        assert!(resolutions[1].applied, "exact-position hunk applies");
        assert_eq!(hunks, 1);
        assert!(result.contains("// banner"));
        assert!(result.contains("let x = 1;"), "first hunk left unapplied");
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
        let (result, hunks, _added, _removed, resolutions) = apply_unified_tiered(
            diff_text,
            original,
            file_path,
            extractor,
            PatchMode::Structural,
        )
        .unwrap();
        assert_eq!(hunks, 1);
        assert!(resolutions[0].applied);
        assert_eq!(resolutions[0].tier_used, 1);
        assert_eq!(resolutions[0].entity_matched.as_deref(), Some("fn target"));
        assert_eq!(resolutions[0].matched_at_line, Some(5));
        assert!(matches!(resolutions[0].confidence, Confidence::High));
        assert!(result.contains("let value = 2;"));
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
        let (result, hunks, _added, _removed, resolutions) = apply_unified_tiered(
            diff_text,
            original,
            file_path,
            extractor,
            PatchMode::Structural,
        )
        .unwrap();
        assert_eq!(hunks, 0);
        assert_eq!(result, original);
        assert!(!resolutions[0].applied);
        assert_eq!(resolutions[0].tier_used, 0);
        let failure = resolutions[0]
            .failure
            .as_ref()
            .expect("failure reason present");
        assert!(
            failure.contains("semantic anchor"),
            "clear message: {failure}",
        );
        // Structural mode reports no structural_alternative (it is itself the
        // structural path).
        assert!(resolutions[0].structural_alternative.is_none());
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
        let (result, hunks, _added, _removed, resolutions) = apply_unified_tiered(
            diff_text,
            original,
            file_path,
            extractor,
            PatchMode::Structural,
        )
        .unwrap();
        assert_eq!(hunks, 0);
        assert_eq!(result, original);
        assert!(!resolutions[0].applied);
        let failure = resolutions[0].failure.as_ref().expect("failure reason");
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
        let (result, hunks, _added, _removed, resolutions) = apply_unified_tiered(
            diff_text,
            original,
            file_path,
            extractor,
            PatchMode::Structural,
        )
        .unwrap();
        assert_eq!(hunks, 0, "no diffy fallback in structural mode");
        assert_eq!(result, original);
        assert!(!resolutions[0].applied);
    }
}
