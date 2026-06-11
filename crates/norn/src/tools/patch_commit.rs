//! Staged-file commit and rollback for `apply_patch`.
//!
//! `execute` stages every block's result in memory as a [`StagedFile`];
//! [`commit_staged`] then writes the whole set to disk in two phases
//! (writes/creates first, deletions last) using atomic temp-file-and-rename
//! commits, rolling back everything already touched on the first failure.

use std::path::PathBuf;

use super::file_commit::commit_file_atomic;
use super::patch_parse::PatchBlockKind;
use crate::error::ToolError;

/// One file's staged patch result, kept in memory until every block has
/// been resolved and validated.
pub(super) struct StagedFile {
    /// Resolved on-disk target path.
    pub(super) path: PathBuf,
    /// Raw original content as read from disk (byte-faithful, used for
    /// rollback). Empty for `Create` blocks.
    pub(super) original: String,
    /// Fully patched content to commit. Empty for `Delete` blocks.
    pub(super) staged: String,
    /// Number of hunks applied to this file.
    pub(super) hunks: usize,
    /// Lines added across this file's hunks.
    pub(super) added: usize,
    /// Lines removed across this file's hunks.
    pub(super) removed: usize,
    /// The block kind driving commit behaviour.
    pub(super) kind: PatchBlockKind,
}

/// A rollback step that failed while unwinding a partially-committed patch.
/// The named file may be left in its patched (post-commit) state on disk.
struct RollbackFailure {
    /// The file whose rollback failed.
    path: PathBuf,
    /// What the rollback step was trying to do.
    operation: &'static str,
    /// The underlying I/O error.
    error: std::io::Error,
}

/// Reverses any disk mutations recorded in `applied` (indices into
/// `staged`). Modify → write original back; Create → unlink the file
/// we just created; Delete → recreate the file from captured before-
/// content. Every step's failure is logged and collected so the caller
/// can surface which files were left in their patched state.
async fn rollback_applied(staged: &[StagedFile], applied: &[usize]) -> Vec<RollbackFailure> {
    let mut failures: Vec<RollbackFailure> = Vec::new();
    for &idx in applied.iter().rev() {
        let Some(s) = staged.get(idx) else {
            // `applied` indices come from enumerating `staged`, so this is
            // unreachable in practice; log rather than panic if the
            // invariant is ever broken.
            tracing::error!(index = idx, "patch rollback: applied index out of range");
            continue;
        };
        let result = match s.kind {
            PatchBlockKind::Modify | PatchBlockKind::Delete => {
                commit_file_atomic(&s.path, s.original.as_bytes())
                    .await
                    .map_err(|e| ("restore original content", e))
            }
            PatchBlockKind::Create => tokio::fs::remove_file(&s.path)
                .await
                .map_err(|e| ("remove created file", e)),
        };
        if let Err((operation, error)) = result {
            tracing::error!(
                path = %s.path.display(),
                operation,
                error = %error,
                "patch rollback step failed; the file may be left in its patched state",
            );
            failures.push(RollbackFailure {
                path: s.path.clone(),
                operation,
                error,
            });
        }
    }
    failures
}

/// Builds the commit error returned to the caller, appending rollback
/// failures to the primary reason. A failed rollback breaks the
/// all-or-nothing contract — files named here may still hold their patched
/// content — so the caller must see it in the tool result rather than only
/// in the logs.
fn commit_error(reason: String, rollback_failures: &[RollbackFailure]) -> ToolError {
    if rollback_failures.is_empty() {
        return ToolError::ExecutionFailed { reason };
    }
    let detail = rollback_failures
        .iter()
        .map(|f| format!("{} ({}: {})", f.path.display(), f.operation, f.error))
        .collect::<Vec<_>>()
        .join("; ");
    ToolError::ExecutionFailed {
        reason: format!(
            "{reason}; rollback also failed for {} file(s): {detail} — these files may be left \
             in their patched state on disk",
            rollback_failures.len(),
        ),
    }
}

/// Commits every staged file to disk.
///
/// Two phases:
///
/// 1. Write `Modify` and `Create` files (creating parent dirs for
///    `Create`) — these may overwrite or add disk state. Every write is
///    atomic (temp file in the same directory + rename, preserving the
///    original's permissions), so a crash or ENOSPC mid-commit never
///    destroys original content.
/// 2. Delete `Delete` files — only after every non-Delete file has
///    committed successfully, so a write failure never leaves the patch
///    half-applied with files already removed.
///
/// On any failure everything already touched is rolled back: Modify
/// reverts to its captured original, Create is unlinked, Delete is
/// restored from captured before-content.
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] naming the file and underlying
/// I/O error; the rollback has already run when the error is returned, and
/// any rollback step that itself failed is named in the error so the caller
/// knows which files may be left in their patched state.
pub(super) async fn commit_staged(staged_files: &[StagedFile]) -> Result<(), ToolError> {
    let mut applied: Vec<usize> = Vec::with_capacity(staged_files.len());

    for (idx, staged) in staged_files.iter().enumerate() {
        if matches!(staged.kind, PatchBlockKind::Delete) {
            continue;
        }
        if matches!(staged.kind, PatchBlockKind::Create)
            && let Some(parent) = staged.path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            let rollback_failures = rollback_applied(staged_files, &applied).await;
            return Err(commit_error(
                format!(
                    "failed to create parent directories for {}: {e}",
                    staged.path.display()
                ),
                &rollback_failures,
            ));
        }
        if let Err(e) = commit_file_atomic(&staged.path, staged.staged.as_bytes()).await {
            let rollback_failures = rollback_applied(staged_files, &applied).await;
            return Err(commit_error(
                format!("failed to write {}: {e}", staged.path.display()),
                &rollback_failures,
            ));
        }
        applied.push(idx);
    }

    for (idx, staged) in staged_files.iter().enumerate() {
        if !matches!(staged.kind, PatchBlockKind::Delete) {
            continue;
        }
        if let Err(e) = tokio::fs::remove_file(&staged.path).await {
            let rollback_failures = rollback_applied(staged_files, &applied).await;
            return Err(commit_error(
                format!("failed to delete {}: {e}", staged.path.display()),
                &rollback_failures,
            ));
        }
        applied.push(idx);
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn staged(path: PathBuf, original: &str, content: &str, kind: PatchBlockKind) -> StagedFile {
        StagedFile {
            path,
            original: original.to_string(),
            staged: content.to_string(),
            hunks: 1,
            added: 1,
            removed: 1,
            kind,
        }
    }

    #[tokio::test]
    async fn rollback_reports_modify_restore_failure() {
        let dir = tempfile::tempdir().unwrap();
        // Restoring into a directory that does not exist must fail and be
        // reported, not silently swallowed.
        let path = dir.path().join("missing-dir").join("file.txt");
        let files = vec![staged(path.clone(), "orig", "new", PatchBlockKind::Modify)];

        let failures = rollback_applied(&files, &[0]).await;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].path, path);
        assert_eq!(failures[0].operation, "restore original content");
        assert_eq!(failures[0].error.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn rollback_reports_create_unlink_failure() {
        let dir = tempfile::tempdir().unwrap();
        // Unlinking a file that does not exist must fail and be reported.
        let path = dir.path().join("never-created.txt");
        let files = vec![staged(path.clone(), "", "new", PatchBlockKind::Create)];

        let failures = rollback_applied(&files, &[0]).await;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].path, path);
        assert_eq!(failures[0].operation, "remove created file");
    }

    #[tokio::test]
    async fn successful_rollback_reports_no_failures() {
        let dir = tempfile::tempdir().unwrap();
        let modify_path = dir.path().join("modify.txt");
        tokio::fs::write(&modify_path, "patched").await.unwrap();
        let create_path = dir.path().join("created.txt");
        tokio::fs::write(&create_path, "created").await.unwrap();

        let files = vec![
            staged(
                modify_path.clone(),
                "orig",
                "patched",
                PatchBlockKind::Modify,
            ),
            staged(create_path.clone(), "", "created", PatchBlockKind::Create),
        ];

        let failures = rollback_applied(&files, &[0, 1]).await;
        assert!(failures.is_empty());
        assert_eq!(
            tokio::fs::read_to_string(&modify_path).await.unwrap(),
            "orig",
            "modify rolled back to original"
        );
        assert!(!create_path.exists(), "created file unlinked");
    }

    #[tokio::test]
    async fn commit_error_names_rollback_failures() {
        let failures = vec![RollbackFailure {
            path: PathBuf::from("/tmp/dirty.txt"),
            operation: "restore original content",
            error: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        }];
        let err = commit_error(
            "failed to write /tmp/other.txt: boom".to_string(),
            &failures,
        );
        let msg = err.to_string();
        assert!(
            msg.contains("failed to write /tmp/other.txt: boom"),
            "{msg}"
        );
        assert!(msg.contains("rollback also failed for 1 file(s)"), "{msg}");
        assert!(msg.contains("/tmp/dirty.txt"), "{msg}");
        assert!(msg.contains("restore original content"), "{msg}");
        assert!(msg.contains("patched state"), "{msg}");
    }

    #[tokio::test]
    async fn commit_error_without_rollback_failures_is_plain() {
        let err = commit_error("failed to write x: boom".to_string(), &[]);
        let msg = err.to_string();
        assert!(msg.contains("failed to write x: boom"), "{msg}");
        assert!(!msg.contains("rollback"), "{msg}");
    }

    #[tokio::test]
    async fn commit_failure_rolls_back_and_keeps_primary_error_clean() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.txt");
        tokio::fs::write(&good, "orig").await.unwrap();
        // Second file's parent does not exist (Modify never creates parents),
        // so its commit fails after the first file already committed.
        let bad = dir.path().join("missing-dir").join("bad.txt");

        let files = vec![
            staged(good.clone(), "orig", "patched", PatchBlockKind::Modify),
            staged(bad.clone(), "orig", "patched", PatchBlockKind::Modify),
        ];

        let err = commit_staged(&files).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("failed to write"), "{msg}");
        assert!(msg.contains("bad.txt"), "{msg}");
        assert!(
            !msg.contains("rollback also failed"),
            "rollback succeeded, so the error stays clean: {msg}"
        );
        assert_eq!(
            tokio::fs::read_to_string(&good).await.unwrap(),
            "orig",
            "first file rolled back to its original content"
        );
    }
}
