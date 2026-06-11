//! Workspace confinement for the file tools.
//!
//! When an embedder sets a confinement root on the
//! [`ToolContext`](crate::tool::context::ToolContext) (via
//! `confine_to_workspace`), every path the read/write/edit/patch tools
//! operate on is checked by [`check_confinement`] after resolution. The
//! check is symlink-aware: the deepest *existing* ancestor of the target is
//! canonicalized (resolving symlinks and `..`/`.` components), the
//! not-yet-existing suffix is re-appended, and the result must stay inside
//! the canonicalized root. Any `..` component in the non-existing suffix is
//! refused outright because it cannot be verified against symlinks.
//!
//! With no root configured the check is a no-op — confinement is strictly
//! opt-in.

use std::path::{Component, Path, PathBuf};

use crate::tool::context::ToolContext;

/// Verifies that `path` (absolute or relative; relative paths are resolved
/// against the agent working directory) stays inside the context's
/// workspace-confinement root.
///
/// Returns `Ok(())` when no root is configured or the path is confined.
///
/// # Errors
///
/// Returns a human-readable refusal reason when the root cannot be
/// canonicalized, the path contains unverifiable `..` traversal beyond the
/// existing filesystem, or the resolved path escapes the root (including
/// escapes through symlinks).
pub(crate) fn check_confinement(ctx: &ToolContext, path: &Path) -> Result<(), String> {
    let Some(root) = ctx.workspace_root() else {
        return Ok(());
    };
    let canonical_root = root.canonicalize().map_err(|e| {
        format!(
            "workspace confinement root {} cannot be canonicalized: {e}",
            root.display()
        )
    })?;

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        ctx.working_dir().join(path)
    };

    let resolved = resolve_symlink_aware(&absolute)?;
    if resolved.starts_with(&canonical_root) {
        Ok(())
    } else {
        Err(format!(
            "path {} resolves to {} which is outside the workspace confinement root {}",
            path.display(),
            resolved.display(),
            canonical_root.display()
        ))
    }
}

/// Canonicalizes the deepest existing ancestor of `absolute` and re-appends
/// the remaining (not-yet-existing) components, refusing `..` in that
/// suffix because it cannot be resolved against symlinks.
fn resolve_symlink_aware(absolute: &Path) -> Result<PathBuf, String> {
    // Find the deepest ancestor that exists on disk. The root path always
    // exists, so the loop terminates with `Some`.
    let existing = absolute
        .ancestors()
        .find(|a| a.exists())
        .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);

    let canonical_base = existing.canonicalize().map_err(|e| {
        format!(
            "failed to canonicalize existing ancestor {} of {}: {e}",
            existing.display(),
            absolute.display()
        )
    })?;

    let suffix = absolute
        .strip_prefix(&existing)
        .map_err(|e| format!("internal path-prefix error for {}: {e}", absolute.display()))?;

    let mut resolved = canonical_base;
    for component in suffix.components() {
        match component {
            Component::ParentDir => {
                return Err(format!(
                    "path {} traverses `..` through a non-existing directory; refusing to resolve it",
                    absolute.display()
                ));
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    Ok(resolved)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::tool::context::SharedWorkingDir;

    fn confined_ctx(root: &Path, working_dir: &Path) -> ToolContext {
        let mut ctx =
            ToolContext::with_working_dir(SharedWorkingDir::new(working_dir.to_path_buf()));
        ctx.confine_to_workspace(root.to_path_buf());
        ctx
    }

    #[test]
    fn unconfined_context_allows_everything() {
        let ctx = ToolContext::empty();
        assert!(check_confinement(&ctx, Path::new("/etc/passwd")).is_ok());
    }

    #[test]
    fn allows_paths_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = confined_ctx(dir.path(), dir.path());
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        assert!(check_confinement(&ctx, &dir.path().join("a.txt")).is_ok());
        // Not-yet-existing file inside the root is also fine.
        assert!(check_confinement(&ctx, &dir.path().join("sub/new.txt")).is_ok());
        // Relative path resolved against the working dir inside the root.
        assert!(check_confinement(&ctx, Path::new("relative.txt")).is_ok());
    }

    #[test]
    fn refuses_dot_dot_escape() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("ws");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(outer.path().join("secret.txt"), "s").unwrap();
        let ctx = confined_ctx(&root, &root);

        let err = check_confinement(&ctx, &root.join("../secret.txt")).unwrap_err();
        assert!(err.contains("outside the workspace"), "{err}");

        // `..` resolved relatively against the working dir is refused too.
        let err = check_confinement(&ctx, Path::new("../secret.txt")).unwrap_err();
        assert!(err.contains("outside the workspace"), "{err}");
    }

    #[test]
    fn refuses_absolute_path_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = confined_ctx(dir.path(), dir.path());
        let err = check_confinement(&ctx, Path::new("/etc/passwd")).unwrap_err();
        assert!(err.contains("outside the workspace"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_escape() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("ws");
        let elsewhere = outer.path().join("elsewhere");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("target.txt"), "t").unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();

        let ctx = confined_ctx(&root, &root);
        let err = check_confinement(&ctx, &root.join("link/target.txt")).unwrap_err();
        assert!(err.contains("outside the workspace"), "{err}");
        // A not-yet-existing file behind the escaping symlink is refused too.
        let err = check_confinement(&ctx, &root.join("link/new.txt")).unwrap_err();
        assert!(err.contains("outside the workspace"), "{err}");
    }

    #[test]
    fn refuses_dot_dot_through_nonexisting_directory() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = confined_ctx(dir.path(), dir.path());
        let sneaky = dir.path().join("no-such-dir/../../../etc/passwd");
        let err = check_confinement(&ctx, &sneaky).unwrap_err();
        assert!(
            err.contains("traverses `..`") || err.contains("outside the workspace"),
            "{err}"
        );
    }

    #[test]
    fn refuses_when_root_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing_root = dir.path().join("gone");
        let ctx = confined_ctx(&missing_root, dir.path());
        let err = check_confinement(&ctx, &dir.path().join("a.txt")).unwrap_err();
        assert!(err.contains("cannot be canonicalized"), "{err}");
    }
}
