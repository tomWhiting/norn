//! Pre-execution gating for `apply_patch`: argument parsing, block-ordering
//! validation, workspace confinement, target existence, and the
//! read-before-edit gate.

use std::collections::HashMap;
use std::path::PathBuf;

use super::confinement::check_confinement;
use super::patch::PatchArgs;
use super::patch_parse::{PatchBlockKind, parse_blocks, resolve_path};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::lifecycle::PreValidateOutcome;

/// The directory patch-relative paths resolve against: the model-supplied
/// `working_dir` when present, otherwise the session working directory.
pub(super) fn effective_working_dir(working_dir: Option<&str>, ctx: &ToolContext) -> PathBuf {
    working_dir.map_or_else(|| ctx.working_dir(), PathBuf::from)
}

/// Full `pre_validate` pipeline for `apply_patch`.
///
/// 1. Parse the arguments and the patch into blocks.
/// 2. Reject patches where a later block modifies or creates a file that an
///    earlier block deletes. Modify→Delete (delete a file after editing it
///    within the same patch) is allowed; Delete→{Modify,Create} on the same
///    path would write to a path the patch just removed.
/// 3. Per block: workspace confinement, existence, and read-before-edit
///    gates. Create blocks skip both gates (the file does not exist yet by
///    definition) but are rejected if the target already exists on disk —
///    that would silently turn a creation into a Modify.
pub(super) fn pre_validate_patch(envelope: &ToolEnvelope, ctx: &ToolContext) -> PreValidateOutcome {
    let args: PatchArgs = match serde_json::from_value(envelope.model_args.clone()) {
        Ok(a) => a,
        Err(e) => {
            return PreValidateOutcome::Block {
                reason: format!("invalid arguments: {e}"),
            };
        }
    };
    let blocks = match parse_blocks(&args.patch) {
        Ok(b) => b,
        Err(e) => {
            return PreValidateOutcome::Block {
                reason: format!("patch parse failed: {e}"),
            };
        }
    };

    let mut first_delete: HashMap<&str, usize> = HashMap::new();
    for (i, block) in blocks.iter().enumerate() {
        if matches!(block.kind, PatchBlockKind::Delete) {
            first_delete.entry(block.target.as_str()).or_insert(i);
            continue;
        }
        if let Some(&del_idx) = first_delete.get(block.target.as_str()) {
            let action = match block.kind {
                PatchBlockKind::Modify => "modified",
                PatchBlockKind::Create => "created",
                PatchBlockKind::Delete => continue,
            };
            return PreValidateOutcome::Block {
                reason: format!(
                    "apply_patch: file '{}' is deleted by block {del_idx} but {action} by block {i}",
                    block.target,
                ),
            };
        }
    }

    let effective_wd = effective_working_dir(args.working_dir.as_deref(), ctx);
    // Workspace confinement: a model-supplied working_dir must itself
    // live inside the configured root, and so must every target.
    if args.working_dir.is_some()
        && let Err(reason) = check_confinement(ctx, &effective_wd)
    {
        return PreValidateOutcome::Block {
            reason: format!("apply_patch: working_dir refused: {reason}"),
        };
    }
    for block in &blocks {
        let path = resolve_path(&effective_wd, &block.target);
        if let Err(reason) = check_confinement(ctx, &path) {
            return PreValidateOutcome::Block {
                reason: format!("apply_patch: target refused: {reason}"),
            };
        }
        match block.kind {
            PatchBlockKind::Create => {
                if path.exists() {
                    return PreValidateOutcome::Block {
                        reason: format!(
                            "apply_patch: Create block target already exists: {}",
                            path.display()
                        ),
                    };
                }
            }
            PatchBlockKind::Modify | PatchBlockKind::Delete => {
                if !path.exists() {
                    return PreValidateOutcome::Block {
                        reason: format!(
                            "apply_patch: target file does not exist: {}",
                            path.display()
                        ),
                    };
                }
                if !ctx.has_read_file(&path) {
                    return PreValidateOutcome::Block {
                        reason: format!(
                            "apply_patch: target file not read this session: {}",
                            path.display()
                        ),
                    };
                }
            }
        }
    }
    PreValidateOutcome::Proceed
}
