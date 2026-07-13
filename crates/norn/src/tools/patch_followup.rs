//! Strict→structural escalation follow-up for `apply_patch`.
//!
//! When a strict-mode run leaves a hunk unapplied that structural matching
//! would have placed, the tool offers an `apply_patch` follow-up that
//! re-runs the same patch with `mode: auto`.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::session::action_log::hash_content;
use crate::tool::follow_up::{BeforeContentSource, Confidence, ExpiryCondition, FollowUpAction};
use crate::tool::traits::ToolOutput;

/// Whether any `resolution_details` entry is an unapplied hunk that carries a
/// non-null `structural_alternative` — i.e. a strict failure that structural
/// matching would have resolved.
pub(super) fn has_viable_strict_alternative(resolution_details: &[serde_json::Value]) -> bool {
    resolution_details.iter().any(|d| {
        d.get("applied").and_then(serde_json::Value::as_bool) == Some(false)
            && d.get("structural_alternative")
                .is_some_and(|v| !v.is_null())
    })
}

/// Builds the strict→structural escalation follow-up from a strict-mode
/// run's output.
///
/// The action only overrides `mode`; the runtime merges it over the
/// original call's args, so the original patch text is reused without
/// re-generation. The follow-up expires if any of the touched/attempted
/// files change before it runs, since that would invalidate the structural
/// placement the alternative reported. Returns an empty vector when the run
/// was not strict-mode or no failed hunk has a viable structural
/// alternative.
pub(super) async fn strict_escalation_follow_ups(output: &ToolOutput) -> Vec<FollowUpAction> {
    if output
        .content
        .get("mode")
        .and_then(serde_json::Value::as_str)
        != Some("strict")
    {
        return Vec::new();
    }
    let Some(details) = output
        .content
        .get("resolution_details")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let viable = details
        .iter()
        .filter(|d| {
            d.get("applied").and_then(serde_json::Value::as_bool) == Some(false)
                && d.get("structural_alternative")
                    .is_some_and(|v| !v.is_null())
        })
        .count();
    if viable == 0 {
        return Vec::new();
    }

    // Key the expiry on the current on-disk hashes of the files this patch
    // touched or attempted. If any changes before the follow-up runs, the
    // reported structural alternative is stale and the action expires.
    let mut file_hashes: HashMap<PathBuf, String> = HashMap::new();
    for key in ["files_modified", "files_attempted"] {
        let Some(arr) = output
            .content
            .get(key)
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for v in arr {
            let Some(path_str) = v.as_str() else { continue };
            let path = PathBuf::from(path_str);
            if file_hashes.contains_key(&path) {
                continue;
            }
            let read_result = {
                let _descriptor_permit = match crate::resource::acquire_filesystem_operation() {
                    Ok(permit) => permit,
                    Err(error) => {
                        tracing::warn!(
                            path = %path.display(),
                            %error,
                            "apply_patch follow-up omitted because descriptor admission failed"
                        );
                        return Vec::new();
                    }
                };
                tokio::fs::read(&path).await
            };
            match read_result {
                Ok(bytes) => {
                    file_hashes.insert(path, hash_content(&bytes));
                }
                Err(e) => {
                    // The file cannot be hashed, so the expiry condition
                    // cannot track it; say so rather than silently weakening
                    // the staleness guarantee for this follow-up.
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "apply_patch follow-up: cannot hash file for expiry tracking; \
                         changes to it will not expire the follow-up",
                    );
                }
            }
        }
    }
    let expires = if file_hashes.is_empty() {
        ExpiryCondition::Never
    } else {
        ExpiryCondition::AnyFileModified { files: file_hashes }
    };

    vec![FollowUpAction {
        action: "apply_structural".to_string(),
        description: format!(
            "Re-apply this patch with structural matching (mode: auto). {viable} hunk(s) failed strict exact-position matching but structural matching would resolve them."
        ),
        tool: "apply_patch".to_string(),
        args: serde_json::json!({ "mode": "auto" }),
        args_mode: crate::tool::follow_up::FollowUpArgsMode::MergeOriginal,
        expires,
        confidence: Confidence::High,
        before_content: BeforeContentSource::Unavailable,
    }]
}
