//! Mutation extraction helpers for the action log.
//!
//! This module maps successful mutation-tool outputs into mutation-ledger
//! records. It is intentionally private to the session module; the public
//! [`ActionLog`](super::action_log::ActionLog) API remains in `action_log`.

use std::path::{Path, PathBuf};

use crate::session::mutation_ledger::{DiffStats, MutationOp, RecordedMutation};
use crate::tool::follow_up::{BeforeContentSource, FollowUpAction};

/// Extract the mutations a successful tool completion implies.
///
/// Model-supplied paths may be relative; they are resolved against
/// `working_dir` — the agent's working directory, **not** the process
/// CWD — so the ledger's revert-baseline hashing reads the file the tool
/// actually mutated.
///
/// Non-mutation tools, unsupported statuses, and unrecognised output shapes
/// yield no mutations.
pub(super) fn extract_mutations(
    tool_name: &str,
    tool_call_id: &str,
    output: &serde_json::Value,
    follow_ups: &[FollowUpAction],
    working_dir: &Path,
) -> Vec<RecordedMutation> {
    match tool_name {
        "edit" => extract_edit(tool_call_id, output, working_dir),
        "write" => extract_write(tool_call_id, output, follow_ups, working_dir),
        "apply_patch" => extract_apply_patch(tool_call_id, output, working_dir),
        _ => Vec::new(),
    }
}

/// Resolve a model-supplied path against the agent working directory.
/// Absolute paths pass through unchanged.
pub(super) fn resolve_against(working_dir: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        working_dir.join(p)
    }
}

/// `edit` rewrites an existing file in place: one `Modified` mutation whose
/// line deltas come from the blast-radius payload.
fn extract_edit(
    tool_call_id: &str,
    output: &serde_json::Value,
    working_dir: &Path,
) -> Vec<RecordedMutation> {
    let Some(path) = output.get("path").and_then(serde_json::Value::as_str) else {
        return Vec::new();
    };
    let blast = output.get("blast_radius");
    let lines_added = blast.map_or(0, |b| json_u32(b, "lines_added"));
    let lines_removed = blast.map_or(0, |b| json_u32(b, "lines_removed"));
    vec![RecordedMutation {
        file_path: resolve_against(working_dir, path),
        operation: MutationOp::Modified,
        tool_call_id: tool_call_id.to_owned(),
        diff_stats: DiffStats {
            lines_added,
            lines_removed,
        },
    }]
}

/// `write` reports the new line count but not whether the file pre-existed.
/// A `StoredContent` follow-up for the path means the file existed; otherwise
/// the write is treated as a creation.
fn extract_write(
    tool_call_id: &str,
    output: &serde_json::Value,
    follow_ups: &[FollowUpAction],
    working_dir: &Path,
) -> Vec<RecordedMutation> {
    let Some(path) = output.get("path").and_then(serde_json::Value::as_str) else {
        return Vec::new();
    };
    let file_path = resolve_against(working_dir, path);
    let new_lines = json_u32(output, "line_count");

    // Tools may key stored before-content by either the raw model path or
    // the resolved one; check both.
    let stored = stored_before_content(follow_ups, &file_path)
        .or_else(|| stored_before_content(follow_ups, Path::new(path)));
    let (operation, diff_stats) = match stored {
        Some(old_content) => {
            let old_lines = u32::try_from(old_content.lines().count()).unwrap_or(u32::MAX);
            (
                MutationOp::Modified,
                DiffStats {
                    lines_added: new_lines.saturating_sub(old_lines),
                    lines_removed: old_lines.saturating_sub(new_lines),
                },
            )
        }
        None => (
            MutationOp::Created,
            DiffStats {
                lines_added: new_lines,
                lines_removed: 0,
            },
        ),
    };

    vec![RecordedMutation {
        file_path,
        operation,
        tool_call_id: tool_call_id.to_owned(),
        diff_stats,
    }]
}

/// `apply_patch` exposes a per-file array with path, status, and line deltas.
fn extract_apply_patch(
    tool_call_id: &str,
    output: &serde_json::Value,
    working_dir: &Path,
) -> Vec<RecordedMutation> {
    let Some(per_file) = output.get("per_file").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut mutations = Vec::with_capacity(per_file.len());
    for file in per_file {
        let Some(path) = file.get("path").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let operation = match file.get("status").and_then(serde_json::Value::as_str) {
            Some("created") => MutationOp::Created,
            Some("modified") => MutationOp::Modified,
            Some("deleted") => MutationOp::Deleted,
            _ => continue,
        };
        mutations.push(RecordedMutation {
            file_path: resolve_against(working_dir, path),
            operation,
            tool_call_id: tool_call_id.to_owned(),
            diff_stats: DiffStats {
                lines_added: json_u32(file, "lines_added"),
                lines_removed: json_u32(file, "lines_removed"),
            },
        });
    }
    mutations
}

/// Find pre-mutation content for `path` in any `StoredContent` follow-up.
fn stored_before_content(follow_ups: &[FollowUpAction], path: &Path) -> Option<String> {
    for follow_up in follow_ups {
        if let BeforeContentSource::StoredContent { files } = &follow_up.before_content
            && let Some(content) = files.get(path)
        {
            return Some(content.clone());
        }
    }
    None
}

/// Read a non-negative integer JSON field as `u32`, defaulting to `0` when the
/// field is absent or out of range.
fn json_u32(value: &serde_json::Value, key: &str) -> u32 {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}
