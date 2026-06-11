//! In-memory staging of `apply_patch` blocks.
//!
//! Each block's result is resolved against disk (or against the staged
//! result of an earlier block targeting the same file) and held as a
//! [`StagedFile`]; nothing touches disk until every block has been staged
//! and AST-validated, at which point `patch_commit::commit_staged` writes
//! the whole set atomically.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::confinement::check_confinement;
use super::patch::PatchMode;
use super::patch_apply::apply_unified_tiered;
use super::patch_cc::{CcHunk, apply_cc_hunks};
use super::patch_commit::StagedFile;
use super::patch_entity::EntityExtractor;
use super::patch_eol::EolInfo;
use super::patch_parse::{
    PatchBlock, PatchBlockKind, PatchFormat, collect_unified_additions, resolve_path,
};
use super::patch_resolve::HunkResolution;
use crate::error::ToolError;
use crate::tool::context::ToolContext;

/// The staged result of every block in a patch, plus aggregate counters and
/// the per-hunk resolution metadata surfaced to the model.
pub(super) struct StagedSet {
    /// One entry per distinct target file, in first-touch order.
    pub(super) files: Vec<StagedFile>,
    /// Hunks applied across all files.
    pub(super) total_hunks: usize,
    /// Lines added across all files.
    pub(super) total_added: usize,
    /// Lines removed across all files.
    pub(super) total_removed: usize,
    /// Per-hunk resolution JSON entries for unified-diff Modify blocks.
    pub(super) resolution_details: Vec<serde_json::Value>,
}

/// One staged block's resolved content and counters, before it is merged
/// into the [`StagedSet`].
struct StagedBlock {
    /// Raw original content (byte-faithful, for rollback). Empty for Create.
    original: String,
    /// Fully patched content. Empty for Delete.
    content: String,
    /// Hunks applied by this block.
    hunks: usize,
    /// Lines added by this block.
    added: usize,
    /// Lines removed by this block.
    removed: usize,
    /// Per-hunk resolutions (unified-diff Modify blocks only).
    resolutions: Vec<HunkResolution>,
}

/// Stages every block of a parsed patch in memory.
///
/// Modify blocks apply against the previous block's staged result when the
/// file was already staged (H8) and otherwise against disk, with EOL style
/// and trailing-newline state preserved (a unified diff's `\ No newline at
/// end of file` markers override the detected trailing-newline state).
/// Create blocks collect the patch's added lines; Delete blocks capture the
/// original content for rollback.
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] when a target escapes workspace
/// confinement, a file cannot be read, or hunk application fails.
pub(super) async fn stage_blocks(
    blocks: Vec<PatchBlock<'_>>,
    effective_wd: &Path,
    ctx: &ToolContext,
    extractor: Option<&dyn EntityExtractor>,
    mode: PatchMode,
) -> Result<StagedSet, ToolError> {
    let mut set = StagedSet {
        files: Vec::with_capacity(blocks.len()),
        total_hunks: 0,
        total_added: 0,
        total_removed: 0,
        resolution_details: Vec::new(),
    };
    // H8: maps a target path to its entry in `set.files`, so a later
    // Modify block for the same file applies against the staged result
    // of the earlier block instead of silently overwriting it.
    let mut staged_index: HashMap<PathBuf, usize> = HashMap::new();

    for block in blocks {
        let path = resolve_path(effective_wd, &block.target);
        if let Err(reason) = check_confinement(ctx, &path) {
            return Err(ToolError::ExecutionFailed {
                reason: format!("apply_patch: target refused: {reason}"),
            });
        }
        let prior = if matches!(block.kind, PatchBlockKind::Modify) {
            staged_index.get(&path).copied()
        } else {
            None
        };

        let staged_block = match block.kind {
            PatchBlockKind::Modify => {
                stage_modify(&block, &path, prior, &set, extractor, mode).await?
            }
            PatchBlockKind::Create => stage_create(&block, &path)?,
            PatchBlockKind::Delete => stage_delete(&path).await?,
        };

        // Record per-hunk resolution metadata for unified-diff hunks (R5).
        // Create and Delete blocks place no hunks against existing content,
        // and the Claude Code format resolves with its own strategy, so
        // neither emits resolution_details entries. Hunk index is 1-based to
        // match the user-facing numbering in error messages.
        for (hunk_index, res) in staged_block.resolutions.iter().enumerate() {
            set.resolution_details.push(serde_json::json!({
                "file": path.to_string_lossy(),
                "hunk_index": hunk_index + 1,
                "tier_used": res.tier_used,
                "entity_matched": res.entity_matched,
                "matched_at_line": res.matched_at_line,
                "stated_line": res.stated_line,
                "drift": res.drift,
                "confidence": res.confidence,
                "applied": res.applied,
                "failure": res.failure,
                "structural_alternative": res.structural_alternative,
            }));
        }

        set.total_hunks += staged_block.hunks;
        set.total_added += staged_block.added;
        set.total_removed += staged_block.removed;

        if let Some(i) = prior {
            // Merge into the earlier staged entry for this file: the new
            // staged content already incorporates the earlier block's
            // changes, and the captured disk original is kept for
            // rollback.
            let entry = &mut set.files[i];
            entry.staged = staged_block.content;
            entry.hunks += staged_block.hunks;
            entry.added += staged_block.added;
            entry.removed += staged_block.removed;
        } else {
            staged_index.insert(path.clone(), set.files.len());
            set.files.push(StagedFile {
                path,
                original: staged_block.original,
                staged: staged_block.content,
                hunks: staged_block.hunks,
                added: staged_block.added,
                removed: staged_block.removed,
                kind: block.kind,
            });
        }
    }

    Ok(set)
}

/// Stages a single Modify block: applies its hunks against the prior staged
/// content (or disk), preserving EOL style and honouring trailing-newline
/// markers.
async fn stage_modify(
    block: &PatchBlock<'_>,
    path: &Path,
    prior: Option<usize>,
    set: &StagedSet,
    extractor: Option<&dyn EntityExtractor>,
    mode: PatchMode,
) -> Result<StagedBlock, ToolError> {
    // Apply against the previous block's staged result when this file was
    // already staged; otherwise read from disk.
    let base_raw = match prior {
        Some(i) => set.files[i].staged.clone(),
        None => tokio::fs::read_to_string(path)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("failed to read {} for patch: {e}", path.display()),
            })?,
    };
    // Preserve the file's EOL style and trailing-newline state: match and
    // patch on LF-normalized text, then restore the original encoding. A
    // unified diff's `\ No newline at end of file` markers override the
    // detected trailing-newline state.
    let mut eol = EolInfo::detect(&base_raw);
    let base = eol.normalize(&base_raw);
    let (applied, hunks, added, removed, resolutions) = match &block.format {
        PatchFormat::UnifiedDiff { raw } => {
            let outcome = apply_unified_tiered(raw, &base, path, extractor, mode)?;
            if let Some(trailing) = outcome.trailing_newline {
                eol = eol.with_trailing_newline(trailing);
            }
            (
                outcome.content,
                outcome.hunks_applied,
                outcome.lines_added,
                outcome.lines_removed,
                outcome.resolutions,
            )
        }
        PatchFormat::ClaudeCode { hunks: cc_hunks } => {
            let applied =
                apply_cc_hunks(&base, cc_hunks).map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("failed to apply patch to {}: {e}", path.display()),
                })?;
            let h = cc_hunks.len();
            let a: usize = cc_hunks.iter().map(CcHunk::addition_count).sum();
            let r: usize = cc_hunks.iter().map(CcHunk::removal_count).sum();
            (applied, h, a, r, Vec::new())
        }
    };
    Ok(StagedBlock {
        original: base_raw,
        content: eol.restore(&applied),
        hunks,
        added,
        removed,
        resolutions,
    })
}

/// Stages a single Create block by collecting the unified diff's additions.
fn stage_create(block: &PatchBlock<'_>, path: &Path) -> Result<StagedBlock, ToolError> {
    // CC format is never tagged Create at parse time (parse_blocks tags
    // every CC block as Modify), but guard the invariant rather than panic.
    let raw = match &block.format {
        PatchFormat::UnifiedDiff { raw } => *raw,
        PatchFormat::ClaudeCode { .. } => {
            return Err(ToolError::ExecutionFailed {
                reason: format!(
                    "apply_patch: Claude Code format does not support file creation: {}",
                    path.display()
                ),
            });
        }
    };
    let (content, hunks, added) = collect_unified_additions(raw);
    Ok(StagedBlock {
        original: String::new(),
        content,
        hunks,
        added,
        removed: 0,
        resolutions: Vec::new(),
    })
}

/// Stages a single Delete block, capturing the original content for rollback.
async fn stage_delete(path: &Path) -> Result<StagedBlock, ToolError> {
    let original =
        tokio::fs::read_to_string(path)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!(
                    "apply_patch: file expected for deletion does not exist: {}: {e}",
                    path.display()
                ),
            })?;
    let removed = original.lines().count();
    Ok(StagedBlock {
        original,
        content: String::new(),
        hunks: 0,
        added: 0,
        removed,
        resolutions: Vec::new(),
    })
}
