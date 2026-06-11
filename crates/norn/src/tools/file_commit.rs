//! Atomic file commit shared by the file-mutating tools (write/edit/patch).
//!
//! All disk commits go through [`commit_file_atomic`]: content is written to
//! a temporary file in the *same directory* as the target, flushed and
//! fsynced, then renamed over the target. A crash, cancellation, or ENOSPC
//! mid-write can therefore never destroy the original content — the target
//! either still holds its previous bytes or holds the complete new bytes.
//!
//! When the target already exists its permission bits are copied onto the
//! temporary file before the rename so the commit does not reset modes.

use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Atomically replaces (or creates) `path` with `bytes`.
///
/// Strategy: resolve symlinks so the commit targets the link's final target
/// (editing through a symlink must rewrite the target, not replace the link
/// with a regular file), then write to `.<file-name>.<uuid>.norn-tmp` in the
/// resolved target's parent directory, fsync it, copy the existing target's
/// permissions onto it (when the target exists), and `rename` over the
/// target. The rename is atomic on POSIX filesystems, so readers never
/// observe a torn file and a failure at any step leaves the original
/// untouched.
///
/// The temporary file is removed on every failure path; a cleanup failure is
/// logged but never masks the original error.
///
/// # Errors
///
/// Returns the underlying I/O error when symlink resolution fails (e.g. a
/// link cycle), the temporary file cannot be created/written/synced,
/// permissions cannot be copied, or the rename fails.
pub(crate) async fn commit_file_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target = resolve_symlink_target(path).await?;
    let tmp = temp_sibling_path(&target)?;

    let result = write_and_rename(&target, &tmp, bytes).await;
    if result.is_err() {
        // The commit already failed; a leftover temp file is cosmetic and
        // must not mask the original error. NotFound is the expected case
        // when the temp file was never created.
        if let Err(cleanup) = tokio::fs::remove_file(&tmp).await
            && cleanup.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %tmp.display(),
                error = %cleanup,
                "failed to remove temporary commit file after a failed commit",
            );
        }
    }
    result
}

/// Resolves `path` to the file the commit must replace, following symlinks
/// so that committing through a link rewrites the link's *target* instead of
/// silently replacing the link with a regular file.
///
/// `fs::canonicalize` handles every fully-existing path and reports link
/// cycles as the OS loop error (`ELOOP`). When it fails with `NotFound` — a
/// fresh file, or a symlink chain ending at a not-yet-existing target — the
/// final chain is walked manually: each *existing* link is read once and its
/// target resolved against the link's parent directory, terminating at the
/// first non-link path. The walk is finite: every step consumes an existing
/// link, and any cycle of existing links has already been reported by
/// `canonicalize` as a non-`NotFound` error.
async fn resolve_symlink_target(path: &Path) -> std::io::Result<PathBuf> {
    match tokio::fs::canonicalize(path).await {
        Ok(resolved) => Ok(resolved),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut current = path.to_path_buf();
            loop {
                let is_link = match tokio::fs::symlink_metadata(&current).await {
                    Ok(meta) => meta.file_type().is_symlink(),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                    Err(e) => return Err(e),
                };
                if !is_link {
                    return Ok(current);
                }
                let link_target = tokio::fs::read_link(&current).await?;
                current = if link_target.is_absolute() {
                    link_target
                } else {
                    match current.parent() {
                        Some(parent) => parent.join(link_target),
                        None => link_target,
                    }
                };
            }
        }
        Err(e) => Err(e),
    }
}

/// Builds the temporary sibling path `.<file-name>.<uuid>.norn-tmp` for
/// `path`, validating that the target has a file name and a parent directory.
fn temp_sibling_path(path: &Path) -> std::io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "cannot commit to a path without a file name: {}",
                path.display()
            ),
        )
    })?;
    let parent = path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let tmp_name = format!(
        ".{}.{}.norn-tmp",
        file_name.to_string_lossy(),
        Uuid::new_v4().simple()
    );
    Ok(parent.join(tmp_name))
}

/// Writes `bytes` to `tmp`, fsyncs, mirrors `path`'s permissions when it
/// exists, then renames `tmp` over `path`.
async fn write_and_rename(path: &Path, tmp: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = tokio::fs::File::create(tmp).await?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    drop(file);

    match tokio::fs::metadata(path).await {
        Ok(meta) => {
            tokio::fs::set_permissions(tmp, meta.permissions()).await?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Fresh file: keep the umask-derived default permissions.
        }
        Err(e) => return Err(e),
    }

    tokio::fs::rename(tmp, path).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn commits_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.txt");
        commit_file_atomic(&path, b"hello").await.unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"hello");
    }

    #[tokio::test]
    async fn replaces_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        tokio::fs::write(&path, "old").await.unwrap();
        commit_file_atomic(&path, b"new").await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "new");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.sh");
        tokio::fs::write(&path, "#!/bin/sh\n").await.unwrap();
        let mut perms = tokio::fs::metadata(&path).await.unwrap().permissions();
        perms.set_mode(0o751);
        tokio::fs::set_permissions(&path, perms).await.unwrap();

        commit_file_atomic(&path, b"#!/bin/sh\necho updated\n")
            .await
            .unwrap();

        let mode = tokio::fs::metadata(&path).await.unwrap().permissions();
        assert_eq!(mode.mode() & 0o7777, 0o751, "permission bits preserved");
    }

    #[tokio::test]
    async fn failure_leaves_original_intact_and_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-parent").join("file.txt");
        // Parent does not exist: temp creation fails, nothing is written.
        let err = commit_file_atomic(&path, b"data").await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        let mut entries = tokio::fs::read_dir(dir.path()).await.unwrap();
        assert!(
            entries.next_entry().await.unwrap().is_none(),
            "no stray files in the parent directory"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_failure_preserves_original_bytes() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        tokio::fs::write(&path, "original").await.unwrap();

        // Read-only directory: the temp file cannot be created, so the
        // commit fails before the original is ever touched. (The legacy
        // in-place `fs::write` would have truncated the file first.)
        let mut perms = tokio::fs::metadata(dir.path()).await.unwrap().permissions();
        perms.set_mode(0o555);
        tokio::fs::set_permissions(dir.path(), perms).await.unwrap();

        let result = commit_file_atomic(&path, b"replacement").await;

        let mut restore = tokio::fs::metadata(dir.path()).await.unwrap().permissions();
        restore.set_mode(0o755);
        tokio::fs::set_permissions(dir.path(), restore)
            .await
            .unwrap();

        result.unwrap_err();
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "original",
            "original content untouched on commit failure"
        );
    }

    #[tokio::test]
    async fn rejects_path_without_file_name() {
        let err = commit_file_atomic(Path::new("/"), b"x").await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn editing_via_symlink_preserves_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        tokio::fs::write(&target, "old").await.unwrap();
        let link = dir.path().join("link.txt");
        tokio::fs::symlink("target.txt", &link).await.unwrap();

        commit_file_atomic(&link, b"new").await.unwrap();

        let meta = tokio::fs::symlink_metadata(&link).await.unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "the symlink must survive the commit"
        );
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "new",
            "the link's target receives the new content"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn editing_via_symlink_chain_preserves_every_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        tokio::fs::write(&target, "old").await.unwrap();
        let inner = dir.path().join("inner.txt");
        tokio::fs::symlink("target.txt", &inner).await.unwrap();
        let outer = dir.path().join("outer.txt");
        tokio::fs::symlink("inner.txt", &outer).await.unwrap();

        commit_file_atomic(&outer, b"new").await.unwrap();

        for link in [&outer, &inner] {
            let meta = tokio::fs::symlink_metadata(link).await.unwrap();
            assert!(meta.file_type().is_symlink(), "{} survives", link.display());
        }
        assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "new");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn editing_via_dangling_symlink_creates_target_and_preserves_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("missing.txt");
        let link = dir.path().join("link.txt");
        tokio::fs::symlink("missing.txt", &link).await.unwrap();

        commit_file_atomic(&link, b"created").await.unwrap();

        let meta = tokio::fs::symlink_metadata(&link).await.unwrap();
        assert!(meta.file_type().is_symlink(), "dangling link survives");
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "created",
            "the dangling link's target is created with the new content"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_loop_is_a_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        tokio::fs::symlink("b.txt", &a).await.unwrap();
        tokio::fs::symlink("a.txt", &b).await.unwrap();

        let err = commit_file_atomic(&a, b"x").await.unwrap_err();
        // The OS reports the cycle (ELOOP); both links are left untouched.
        assert!(
            tokio::fs::symlink_metadata(&a)
                .await
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!err.to_string().is_empty());
    }
}
