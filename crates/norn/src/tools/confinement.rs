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
//!
//! # Read-class carve-out (DECISIONS §0.6(b))
//!
//! Write/edit/patch always use [`check_confinement`], which never looks
//! beyond the root. Read-class consumers (the read, search, and LSP tools)
//! use [`check_read_confinement`], which additionally admits a resolved
//! path that lands under one of the context's canonicalized
//! [`read_exempt_roots`](crate::tool::context::ToolContext::read_exempt_roots)
//! — the operator-configured skill / profile / config directories. Both
//! entry points resolve the target with the same symlink-aware
//! canonicalization, so an exemption can only ever be granted for a path
//! that genuinely resolves inside an exempt root.

use std::path::{Component, Path, PathBuf};

use crate::tool::context::ToolContext;

/// Verifies that `path` (absolute or relative; relative paths are resolved
/// against the agent working directory) stays inside the context's
/// workspace-confinement root.
///
/// This is the **write-class** check: it never consults the read-exempt
/// roots. Use [`check_read_confinement`] for read-only access.
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
    let Some((resolved, canonical_root)) = resolve_against_root(ctx, path)? else {
        return Ok(());
    };
    if resolved.starts_with(&canonical_root) {
        Ok(())
    } else {
        Err(escape_reason(path, &resolved, &canonical_root))
    }
}

/// The **read-class** confinement check: like [`check_confinement`], but a
/// resolved path that escapes the workspace root is still admitted when it
/// lands under one of the context's canonicalized read-exempt roots
/// (owner-approved carve-out, DECISIONS §0.6(b)).
///
/// The exemption operates on the same symlink-aware resolved path the
/// write-class check uses, so `..`-traversal cannot fabricate it and a
/// symlink inside the workspace that points at an exempt dir resolves
/// (correctly) as exempt.
///
/// # Errors
///
/// As [`check_confinement`], except an escape that resolves under an
/// exempt root returns `Ok(())` instead of a refusal.
pub(crate) fn check_read_confinement(ctx: &ToolContext, path: &Path) -> Result<(), String> {
    let Some((resolved, canonical_root)) = resolve_against_root(ctx, path)? else {
        return Ok(());
    };
    if resolved.starts_with(&canonical_root) {
        return Ok(());
    }
    if ctx
        .read_exempt_roots()
        .iter()
        .any(|exempt| resolved.starts_with(exempt))
    {
        return Ok(());
    }
    Err(escape_reason(path, &resolved, &canonical_root))
}

/// Resolve `path` against the confinement root, returning the
/// symlink-aware resolved path alongside the canonicalized root. Returns
/// `Ok(None)` when no root is configured (confinement is opt-in).
fn resolve_against_root(
    ctx: &ToolContext,
    path: &Path,
) -> Result<Option<(PathBuf, PathBuf)>, String> {
    let Some(root) = ctx.workspace_root() else {
        return Ok(None);
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
    Ok(Some((resolved, canonical_root)))
}

/// The uniform refusal message for a path that resolves outside the root.
fn escape_reason(path: &Path, resolved: &Path, canonical_root: &Path) -> String {
    format!(
        "path {} resolves to {} which is outside the workspace confinement root {}",
        path.display(),
        resolved.display(),
        canonical_root.display()
    )
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

    fn confined_ctx_with_exempt(root: &Path, working_dir: &Path, exempt: &Path) -> ToolContext {
        let mut ctx = confined_ctx(root, working_dir);
        ctx.set_read_exempt_roots(vec![exempt.to_path_buf()]);
        ctx
    }

    /// DECISIONS §0.6(b): a file inside a read-exempt root (a home-level
    /// skill / profile / config dir) is READABLE under confinement even
    /// though it is outside the workspace root — but the same path stays
    /// WRITE-refused, because write never consults the exemption.
    #[test]
    fn read_exemption_allows_read_but_not_write_outside_root() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("ws");
        let skills = outer.path().join("home-skills");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&skills).unwrap();
        let companion = skills.join("SKILL.md");
        std::fs::write(&companion, "x").unwrap();

        let ctx = confined_ctx_with_exempt(&root, &root, &skills);

        // Read-class: admitted via the carve-out.
        assert!(
            check_read_confinement(&ctx, &companion).is_ok(),
            "a file inside an exempt skill dir must be readable under confinement",
        );
        // A not-yet-existing file inside the exempt dir is also readable
        // (its deepest existing ancestor resolves under the exempt root).
        assert!(check_read_confinement(&ctx, &skills.join("nested/new.md")).is_ok());

        // Write-class: still refused — the exemption is read-only.
        let err = check_confinement(&ctx, &companion).unwrap_err();
        assert!(err.contains("outside the workspace"), "{err}");
    }

    /// The exemption never widens read access beyond the declared roots: a
    /// path outside both the workspace root and every exempt root is still
    /// refused by the read-class check.
    #[test]
    fn read_exemption_still_refuses_non_exempt_outside_path() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("ws");
        let skills = outer.path().join("home-skills");
        let secret = outer.path().join("secret");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&skills).unwrap();
        std::fs::create_dir(&secret).unwrap();
        std::fs::write(secret.join("data.txt"), "s").unwrap();

        let ctx = confined_ctx_with_exempt(&root, &root, &skills);

        let err = check_read_confinement(&ctx, &secret.join("data.txt")).unwrap_err();
        assert!(err.contains("outside the workspace"), "{err}");
        // /etc/passwd is neither the root nor exempt.
        assert!(check_read_confinement(&ctx, Path::new("/etc/passwd")).is_err());
    }

    /// The exemption is decided on the canonical resolved path, so a
    /// `..`-traversal from the workspace toward the exempt dir is admitted
    /// only when it genuinely resolves under the exempt root — exactly the
    /// same canonical rule the write-class check uses, so `..` cannot
    /// fabricate access to a non-exempt sibling.
    #[test]
    fn read_exemption_resolves_dotdot_by_canonical_rule() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("ws");
        let skills = outer.path().join("home-skills");
        let secret = outer.path().join("secret");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&skills).unwrap();
        std::fs::create_dir(&secret).unwrap();
        std::fs::write(skills.join("SKILL.md"), "x").unwrap();
        std::fs::write(secret.join("data.txt"), "s").unwrap();

        let ctx = confined_ctx_with_exempt(&root, &root, &skills);

        // `ws/../home-skills/SKILL.md` canonically resolves under the exempt
        // root → admitted for read.
        assert!(
            check_read_confinement(&ctx, &root.join("../home-skills/SKILL.md")).is_ok(),
            "a `..` path that resolves under the exempt root is admitted",
        );
        // `ws/../secret/data.txt` resolves outside every exempt root → still
        // refused: the exemption cannot be widened by traversal.
        let err = check_read_confinement(&ctx, &root.join("../secret/data.txt")).unwrap_err();
        assert!(err.contains("outside the workspace"), "{err}");
    }

    /// A symlink *inside* the workspace pointing at an exempt dir resolves
    /// (correctly) as exempt for read — the check operates on the resolved
    /// target, so an in-workspace link to an operator-configured dir is
    /// fine, while write still refuses it.
    #[cfg(unix)]
    #[test]
    fn read_exemption_follows_symlink_into_exempt_dir() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("ws");
        let skills = outer.path().join("home-skills");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&skills).unwrap();
        std::fs::write(skills.join("SKILL.md"), "x").unwrap();
        std::os::unix::fs::symlink(&skills, root.join("link")).unwrap();

        let ctx = confined_ctx_with_exempt(&root, &root, &skills);

        assert!(
            check_read_confinement(&ctx, &root.join("link/SKILL.md")).is_ok(),
            "an in-workspace symlink into the exempt dir resolves exempt for read",
        );
        // Write through the same link is refused (read-only carve-out).
        let err = check_confinement(&ctx, &root.join("link/SKILL.md")).unwrap_err();
        assert!(err.contains("outside the workspace"), "{err}");
    }

    /// With no exemptions declared, the read-class check behaves exactly
    /// like the write-class check — the carve-out is strictly additive.
    #[test]
    fn read_confinement_without_exemptions_matches_write_check() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("ws");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();
        std::fs::write(outer.path().join("secret.txt"), "s").unwrap();
        let ctx = confined_ctx(&root, &root);

        assert!(check_read_confinement(&ctx, &root.join("a.txt")).is_ok());
        assert!(check_read_confinement(&ctx, &root.join("../secret.txt")).is_err());
    }

    /// The setter canonicalizes and drops non-existent roots (a
    /// non-existent dir grants no readable file), and dedups.
    #[test]
    fn set_read_exempt_roots_canonicalizes_and_drops_missing() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let missing = dir.path().join("missing");

        let mut ctx = ToolContext::empty();
        ctx.set_read_exempt_roots(vec![real.clone(), missing, real.clone()]);

        let roots = ctx.read_exempt_roots();
        assert_eq!(
            roots.len(),
            1,
            "missing dropped, duplicate deduped: {roots:?}"
        );
        assert_eq!(roots[0], real.canonicalize().unwrap());
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
