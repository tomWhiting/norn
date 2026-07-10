//! Discovery and loading of always-on `NORN.md` context files.
//!
//! Two locations are read at session start (DESIGN.md §D1):
//!
//! 1. `~/.norn/NORN.md` — user-level conventions, shared across every
//!    project. Path resolved via
//!    [`crate::config::paths::norn_dir`] so `$NORN_HOME` overrides
//!    work consistently.
//! 2. `{cwd}/NORN.md` — project-root conventions, committed to the
//!    repo. `cwd` is passed in by the caller; the loader never reaches
//!    for [`std::env::current_dir`] itself.
//!
//! Both files are optional. A missing file is silently skipped (debug
//! logged); other IO errors are warning-logged and treated as "no
//! context for this layer". The loader records the mtime observed at
//! read time so the staleness check ([`ContextLoader::check_staleness`])
//! has a snapshot to compare against.
//! Project files are opened relative to the workspace without following
//! symlinks and must be regular files; the trusted user layer retains normal
//! filesystem semantics.
//!
//! Out of scope for this brief: nested-directory scanning (NX-004) and
//! `build_runtime` wiring (NX-005). NX-002 adds the
//! [`ContextLoader::check_staleness`] re-stat helper and the
//! [`crate::agent_loop::loop_context::LoopContext::refresh_context_if_stale`]
//! integration point; the actual rebuild of `system_sections[0]` lands
//! with NX-005's wiring.

use std::path::{Path, PathBuf};

use crate::context::types::ContextFile;
use crate::util::{read_workspace_text_file, workspace_file_mtime};

/// Filename of the always-on context file at both the user-level and
/// project-root locations.
///
/// Uppercase, with a `.md` extension, deliberately mirroring Claude
/// Code's `CLAUDE.md` so the file is conspicuous in directory
/// listings and stands apart from rule files under `.norn/rules/`.
const NORN_MD: &str = "NORN.md";

/// Inter-file separator used when concatenating the two layers into a
/// single string for [`ContextLoader::formatted_context`]. Matches the
/// `"\n\n"` separator [`crate::agent_loop::loop_context::LoopContext::system_instruction`]
/// uses to join `system_sections`.
const SECTION_SEPARATOR: &str = "\n\n";

/// Holder for the two always-on context layers.
///
/// Construct with [`ContextLoader::load`]. The loader owns the two
/// `Option<ContextFile>` slots, the cwd used for project-layer lookup,
/// a formatting helper, and a [`ContextLoader::check_staleness`] hook
/// that re-stats both layers between iterations. `build_runtime` wiring
/// (NX-005) and nested-directory scanning (NX-004) land in later briefs.
#[derive(Clone, Debug, Default)]
pub struct ContextLoader {
    /// `~/.norn/NORN.md` content and mtime, when present.
    ///
    /// `None` when the file does not exist, when `$NORN_HOME` / home
    /// resolution failed, or when reading the file failed (the failure
    /// is logged at `tracing::warn!`).
    pub user: Option<ContextFile>,
    /// `{cwd}/NORN.md` content and mtime, when present.
    ///
    /// `None` when the file does not exist or reading failed (logged at
    /// `tracing::warn!`).
    pub project: Option<ContextFile>,
    /// Working directory the loader was constructed against.
    ///
    /// Retained so [`ContextLoader::check_staleness`] can re-resolve the
    /// project-layer path without the caller having to pass `cwd` in on
    /// every iteration. The user-layer path is re-resolved through
    /// [`crate::config::paths::norn_dir`] on each call so a mid-session
    /// `$NORN_HOME` change is honoured automatically.
    pub cwd: PathBuf,
}

impl ContextLoader {
    /// Load both always-on `NORN.md` layers, in the order
    /// user-level → project-root.
    ///
    /// Reads `~/.norn/NORN.md` (via
    /// [`crate::config::paths::norn_dir`]) and `{cwd}/NORN.md`. Each is
    /// optional: a missing file is debug-logged and produces `None` in
    /// the corresponding slot; an unreadable file (permissions, etc.)
    /// is warning-logged and likewise produces `None` so a single
    /// broken file never breaks session startup.
    ///
    /// `cwd` is supplied by the caller — typically the workspace root
    /// resolved by `build_runtime` (NX-006). The loader never calls
    /// [`std::env::current_dir`] itself; passing the path in keeps the
    /// loader trivially testable with `tempfile::tempdir`.
    #[must_use]
    pub fn load(cwd: &Path) -> Self {
        let cwd = cwd.canonicalize().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "failed to resolve context workspace root; project context will be unavailable"
            );
            cwd.to_path_buf()
        });
        Self::load_at_launch_root(&cwd)
    }

    /// Loads context from an already-canonical immutable launch root.
    pub(crate) fn load_at_launch_root(cwd: &Path) -> Self {
        let user = user_norn_md_path().and_then(read_context_file);
        let project = read_workspace_context_file(cwd, Path::new(NORN_MD));
        Self {
            user,
            project,
            cwd: cwd.to_path_buf(),
        }
    }

    /// Re-stat both always-on context files and re-read any that have
    /// changed since the last load (or last successful staleness check).
    ///
    /// Returns `true` when at least one layer changed — appeared,
    /// disappeared, or its content was rewritten on disk. The caller
    /// (NX-005's wiring) uses that signal to rebuild `system_sections[0]`
    /// so the iteration sees the new content. Returns `false` when both
    /// layers are unchanged (the common case — two `stat` syscalls per
    /// iteration with no follow-up read).
    ///
    /// A [`ContextFile`] with `mtime: None` is treated as "always
    /// changed" (matches the contract documented on
    /// [`crate::context::types::ContextFile::mtime`]). A previously
    /// present file that has been deleted clears the slot to `None`;
    /// a file that appears for the first time mid-session is loaded
    /// into the corresponding slot.
    ///
    /// Reading failures other than `NotFound` are warning-logged inside
    /// `read_context_file` and clear the slot — the consumer rebuilds
    /// with the absent layer rather than retaining stale content.
    pub fn check_staleness(&mut self) -> bool {
        let user_path = user_norn_md_path();
        let user_changed = match user_path {
            Some(path) => Self::refresh_layer(&mut self.user, &path),
            None => {
                // `$NORN_HOME` and `dirs::home_dir` both failed — there
                // is nowhere to read a user-level layer from. If we had
                // one cached, that's stale and must be cleared.
                if self.user.is_some() {
                    self.user = None;
                    true
                } else {
                    false
                }
            }
        };

        let project_changed =
            Self::refresh_workspace_layer(&mut self.project, &self.cwd, Path::new(NORN_MD));

        user_changed || project_changed
    }

    /// Refresh a single layer in place. Returns `true` when the layer
    /// changed (re-read, cleared, or newly populated). Returns `false`
    /// when the layer's state matches the prior observation.
    fn refresh_layer(slot: &mut Option<ContextFile>, path: &Path) -> bool {
        let on_disk_mtime = match std::fs::metadata(path) {
            Ok(meta) => match meta.modified() {
                Ok(m) => Some(Some(m)),
                Err(_) => Some(None),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                tracing::warn!(
                    "Failed to stat context file {} during staleness check: {e}",
                    path.display(),
                );
                // Treat any non-NotFound stat error as "layer is no longer
                // observable" — clear the slot if we had one, otherwise
                // it stays absent. This matches the policy applied at
                // load time where read failures clear the layer.
                if slot.is_some() {
                    *slot = None;
                    return true;
                }
                return false;
            }
        };

        match (slot.as_ref(), on_disk_mtime) {
            (None, None) => false,
            (Some(_), None) => {
                // File was present, now missing — clear the slot.
                *slot = None;
                true
            }
            (None, Some(_)) => {
                // File appeared mid-session — load it.
                *slot = read_context_file(path.to_path_buf());
                slot.is_some()
            }
            (Some(prev), Some(current)) => {
                // Two cases:
                //   - `prev.mtime` is `None`: contract says "always
                //     re-read". `current.is_none()` and `current.is_some()`
                //     both fall through to the re-read branch.
                //   - `prev.mtime` is `Some`: compare against the
                //     current mtime — re-read on mismatch.
                let changed = match (prev.mtime, current) {
                    (Some(a), Some(b)) => a != b,
                    _ => true,
                };
                if changed {
                    *slot = read_context_file(path.to_path_buf());
                }
                changed
            }
        }
    }

    fn refresh_workspace_layer(
        slot: &mut Option<ContextFile>,
        root: &Path,
        relative: &Path,
    ) -> bool {
        let on_disk_mtime = match workspace_file_mtime(root, relative) {
            Ok(mtime) => Some(mtime),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                tracing::warn!(
                    "Refusing workspace context file {} during staleness check: {error}",
                    root.join(relative).display(),
                );
                if slot.is_some() {
                    *slot = None;
                    return true;
                }
                return false;
            }
        };

        match (slot.as_ref(), on_disk_mtime) {
            (None, None) => false,
            (Some(_), None) => {
                *slot = None;
                true
            }
            (None, Some(_)) => {
                *slot = read_workspace_context_file(root, relative);
                slot.is_some()
            }
            (Some(previous), Some(current)) => {
                let changed = match (previous.mtime, current) {
                    (Some(a), Some(b)) => a != b,
                    _ => true,
                };
                if changed {
                    *slot = read_workspace_context_file(root, relative);
                }
                changed
            }
        }
    }

    /// Format the loaded layers as one string ready for appending to
    /// `system_sections[0]`.
    ///
    /// User-level content appears first, project-root second, separated
    /// by `"\n\n"` (DESIGN.md §D1: project-root content goes later so
    /// the model reads it most recently and project specifics take
    /// effective precedence). When neither file is present an empty
    /// string is returned — callers can append unconditionally without
    /// branching on existence.
    #[must_use]
    pub fn formatted_context(&self) -> String {
        match (self.user.as_ref(), self.project.as_ref()) {
            (Some(u), Some(p)) => format!("{}{SECTION_SEPARATOR}{}", u.content, p.content),
            (Some(u), None) => u.content.clone(),
            (None, Some(p)) => p.content.clone(),
            (None, None) => String::new(),
        }
    }
}

/// Resolve the user-level `NORN.md` path: `{norn_dir()}/NORN.md`.
///
/// Returns [`None`] when [`crate::config::paths::norn_dir`] cannot
/// resolve a directory (no `$NORN_HOME`, no home directory) — the
/// loader treats that as "no user layer available" and continues with
/// the project layer only. Recomputed on every staleness check so a
/// mid-session `$NORN_HOME` change takes effect on the next iteration.
fn user_norn_md_path() -> Option<PathBuf> {
    crate::config::paths::norn_dir().map(|d| d.join(NORN_MD))
}

/// Attempt to read a single `NORN.md` file into a [`ContextFile`].
///
/// Returns `None` when the file does not exist (debug-logged so an
/// absent `NORN.md` does not produce warning noise — the file is
/// optional by design). Returns `None` and logs at `tracing::warn!`
/// for any other IO failure so the operator can still see permission
/// or encoding problems without crashing session startup.
///
/// Modification time is captured via `metadata().modified()`; platforms
/// or filesystems that do not report an mtime produce a [`ContextFile`]
/// with `mtime: None`, which NX-002's staleness check treats as
/// "always re-read".
fn read_context_file(path: PathBuf) -> Option<ContextFile> {
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!("No context file at {}", path.display());
            return None;
        }
        Err(e) => {
            tracing::warn!("Failed to read context file {}: {e}", path.display());
            return None;
        }
    };
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    Some(ContextFile {
        path,
        content,
        mtime,
    })
}

fn read_workspace_context_file(root: &Path, relative: &Path) -> Option<ContextFile> {
    let path = root.join(relative);
    let loaded = match read_workspace_text_file(root, relative) {
        Ok(loaded) => loaded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!("No context file at {}", path.display());
            return None;
        }
        Err(error) => {
            tracing::warn!(
                "Refusing workspace context file {}: {error}",
                path.display(),
            );
            return None;
        }
    };
    Some(ContextFile {
        path,
        content: loaded.content,
        mtime: loaded.modified,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use std::path::Path;

    use super::*;

    /// Environment variable used by [`crate::config::paths::norn_dir`]
    /// to override the user-level root. Kept private — guard-scoped
    /// here only because the loader's `user` layer goes through that
    /// helper, so tests must control it the same way.
    const NORN_HOME: &str = "NORN_HOME";

    /// Guard that swaps `NORN_HOME` for the duration of a test and
    /// restores the prior value on drop. Mirrors the
    /// `NornHomeGuard` pattern in `config/paths.rs` so the `serial`
    /// discipline (and the `unsafe` env mutation it relies on) is
    /// applied consistently. Pair every test that constructs one with
    /// `#[serial_test::serial]`.
    struct NornHomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl NornHomeGuard {
        fn set(value: Option<&Path>) -> Self {
            let prior = std::env::var_os(NORN_HOME);
            // SAFETY: callers carry `#[serial_test::serial]`, so no
            // other thread observes the mutated env. The original
            // value is restored on drop.
            match value {
                Some(path) => unsafe { std::env::set_var(NORN_HOME, path) },
                None => unsafe { std::env::remove_var(NORN_HOME) },
            }
            Self { prior }
        }
    }

    impl Drop for NornHomeGuard {
        fn drop(&mut self) {
            // SAFETY: same serial-test discipline as `Self::set`.
            match &self.prior {
                Some(val) => unsafe { std::env::set_var(NORN_HOME, val) },
                None => unsafe { std::env::remove_var(NORN_HOME) },
            }
        }
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    // ── load: presence / absence ───────────────────────────────────────

    #[test]
    #[serial_test::serial]
    fn load_returns_neither_when_both_files_absent() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));

        let loader = ContextLoader::load(cwd.path());
        assert!(loader.user.is_none());
        assert!(loader.project.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn load_reads_only_user_when_project_absent() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        write(&home.path().join("NORN.md"), "user-conventions");

        let loader = ContextLoader::load(cwd.path());
        let user = loader.user.as_ref().expect("user layer must be loaded");
        assert_eq!(user.content, "user-conventions");
        assert_eq!(user.path, home.path().join("NORN.md"));
        assert!(user.mtime.is_some(), "mtime must be recorded on load");
        assert!(loader.project.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn load_reads_only_project_when_user_absent() -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        write(&cwd.path().join("NORN.md"), "project-conventions");

        let loader = ContextLoader::load(cwd.path());
        assert!(loader.user.is_none());
        let project = loader.project.as_ref().expect("project layer must load");
        assert_eq!(project.content, "project-conventions");
        assert_eq!(project.path, cwd.path().canonicalize()?.join("NORN.md"));
        assert!(project.mtime.is_some(), "mtime must be recorded on load");
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn load_reads_both_when_both_files_present() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        write(&home.path().join("NORN.md"), "user-body");
        write(&cwd.path().join("NORN.md"), "project-body");

        let loader = ContextLoader::load(cwd.path());
        assert_eq!(loader.user.as_ref().unwrap().content, "user-body");
        assert_eq!(loader.project.as_ref().unwrap().content, "project-body");
        assert!(loader.user.as_ref().unwrap().mtime.is_some());
        assert!(loader.project.as_ref().unwrap().mtime.is_some());
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn project_context_symlink_is_refused_without_reading_target()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let outside = tempfile::NamedTempFile::new()?;
        std::fs::write(outside.path(), "sentinel-private-context")?;
        symlink(outside.path(), cwd.path().join(NORN_MD))?;
        let norn_home_guard = NornHomeGuard::set(Some(home.path()));

        let loader = ContextLoader::load(cwd.path());

        assert!(loader.project.is_none());
        assert!(
            !loader
                .formatted_context()
                .contains("sentinel-private-context")
        );
        drop(norn_home_guard);
        Ok(())
    }

    // ── formatted_context: four ordering cases ─────────────────────────

    #[test]
    #[serial_test::serial]
    fn formatted_context_empty_when_neither_present() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));

        let loader = ContextLoader::load(cwd.path());
        assert_eq!(loader.formatted_context(), "");
    }

    #[test]
    #[serial_test::serial]
    fn formatted_context_only_user_returns_user_content_alone() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        write(&home.path().join("NORN.md"), "user-only");

        let loader = ContextLoader::load(cwd.path());
        assert_eq!(loader.formatted_context(), "user-only");
    }

    #[test]
    #[serial_test::serial]
    fn formatted_context_only_project_returns_project_content_alone() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        write(&cwd.path().join("NORN.md"), "project-only");

        let loader = ContextLoader::load(cwd.path());
        assert_eq!(loader.formatted_context(), "project-only");
    }

    #[test]
    #[serial_test::serial]
    fn formatted_context_both_present_places_user_first_then_project() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        write(&home.path().join("NORN.md"), "USER");
        write(&cwd.path().join("NORN.md"), "PROJECT");

        let loader = ContextLoader::load(cwd.path());
        assert_eq!(loader.formatted_context(), "USER\n\nPROJECT");
    }

    // ── path-resolution boundaries ────────────────────────────────────

    /// Even with `NORN_HOME` pointing at a real directory, an absent
    /// `NORN.md` inside that directory must not populate `user` —
    /// confirms the lookup is keyed off the file, not the directory.
    #[test]
    #[serial_test::serial]
    fn load_does_not_invent_user_layer_for_empty_norn_home() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));

        let loader = ContextLoader::load(cwd.path());
        assert!(loader.user.is_none());
    }

    /// Project layer must read from `{cwd}/NORN.md` exactly — not from
    /// `{cwd}/.norn/NORN.md`. Mirrors Claude Code's `CLAUDE.md`
    /// placement at the project root.
    #[test]
    #[serial_test::serial]
    fn load_ignores_norn_md_inside_dot_norn_subdir() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        write(
            &cwd.path().join(".norn").join("NORN.md"),
            "should-be-ignored",
        );

        let loader = ContextLoader::load(cwd.path());
        assert!(
            loader.project.is_none(),
            "NORN.md inside .norn/ must not satisfy the project layer"
        );
    }

    // ── check_staleness: R1–R3 ────────────────────────────────────────

    /// Bump a file's mtime forward by writing fresh content and forcing
    /// a future-timestamp via `set_modified`. Avoids relying on
    /// filesystem-clock granularity (some platforms truncate to whole
    /// seconds, so a same-second rewrite can keep mtime identical).
    fn rewrite_and_advance_mtime(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open for mtime bump");
        let future = std::time::SystemTime::now() + std::time::Duration::from_mins(1);
        file.set_modified(future).expect("set_modified");
    }

    #[test]
    #[serial_test::serial]
    fn check_staleness_returns_false_when_no_changes() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        write(&home.path().join("NORN.md"), "user");
        write(&cwd.path().join("NORN.md"), "project");

        let mut loader = ContextLoader::load(cwd.path());
        assert!(
            !loader.check_staleness(),
            "no mtime change must return false"
        );
        // Content is unchanged.
        assert_eq!(loader.user.as_ref().unwrap().content, "user");
        assert_eq!(loader.project.as_ref().unwrap().content, "project");
    }

    #[test]
    #[serial_test::serial]
    fn check_staleness_returns_false_when_both_files_absent() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));

        let mut loader = ContextLoader::load(cwd.path());
        assert!(
            !loader.check_staleness(),
            "no files present, no observable change"
        );
        assert!(loader.user.is_none());
        assert!(loader.project.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn check_staleness_rereads_changed_user_layer() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        let user_path = home.path().join("NORN.md");
        write(&user_path, "original");

        let mut loader = ContextLoader::load(cwd.path());
        rewrite_and_advance_mtime(&user_path, "updated");

        assert!(
            loader.check_staleness(),
            "user mtime change must return true"
        );
        assert_eq!(loader.user.as_ref().unwrap().content, "updated");
        assert!(loader.project.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn check_staleness_rereads_changed_project_layer() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        let project_path = cwd.path().join("NORN.md");
        write(&project_path, "original");

        let mut loader = ContextLoader::load(cwd.path());
        rewrite_and_advance_mtime(&project_path, "updated");

        assert!(
            loader.check_staleness(),
            "project mtime change must return true"
        );
        assert_eq!(loader.project.as_ref().unwrap().content, "updated");
        assert!(loader.user.is_none());
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn staleness_refresh_refuses_project_context_replaced_by_symlink()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let outside = tempfile::NamedTempFile::new()?;
        std::fs::write(outside.path(), "sentinel-private-refresh")?;
        let project_path = cwd.path().join(NORN_MD);
        std::fs::write(&project_path, "safe context")?;
        let norn_home_guard = NornHomeGuard::set(Some(home.path()));
        let mut loader = ContextLoader::load(cwd.path());
        std::fs::remove_file(&project_path)?;
        symlink(outside.path(), &project_path)?;

        assert!(loader.check_staleness());
        assert!(loader.project.is_none());
        assert!(
            !loader
                .formatted_context()
                .contains("sentinel-private-refresh")
        );
        drop(norn_home_guard);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn check_staleness_leaves_unchanged_slot_untouched() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        let user_path = home.path().join("NORN.md");
        let project_path = cwd.path().join("NORN.md");
        write(&user_path, "user-stable");
        write(&project_path, "project-original");

        let mut loader = ContextLoader::load(cwd.path());
        let prior_user_mtime = loader.user.as_ref().unwrap().mtime;
        rewrite_and_advance_mtime(&project_path, "project-updated");

        assert!(loader.check_staleness());
        // The user slot retains its original content and mtime.
        assert_eq!(loader.user.as_ref().unwrap().content, "user-stable");
        assert_eq!(loader.user.as_ref().unwrap().mtime, prior_user_mtime);
        // The project slot was re-read.
        assert_eq!(loader.project.as_ref().unwrap().content, "project-updated");
    }

    #[test]
    #[serial_test::serial]
    fn check_staleness_clears_deleted_file() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        let project_path = cwd.path().join("NORN.md");
        write(&project_path, "to-be-deleted");

        let mut loader = ContextLoader::load(cwd.path());
        assert!(loader.project.is_some(), "precondition: project loaded");

        std::fs::remove_file(&project_path).unwrap();
        assert!(
            loader.check_staleness(),
            "deletion must register as a change"
        );
        assert!(loader.project.is_none(), "deleted file must clear the slot");
    }

    #[test]
    #[serial_test::serial]
    fn check_staleness_loads_newly_appearing_file() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));

        let mut loader = ContextLoader::load(cwd.path());
        assert!(loader.project.is_none(), "precondition: no project file");

        write(&cwd.path().join("NORN.md"), "mid-session");
        assert!(
            loader.check_staleness(),
            "newly-appearing file must register as a change"
        );
        assert_eq!(loader.project.as_ref().unwrap().content, "mid-session");
    }

    #[test]
    #[serial_test::serial]
    fn check_staleness_updates_mtime_after_reread() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        let project_path = cwd.path().join("NORN.md");
        write(&project_path, "v1");

        let mut loader = ContextLoader::load(cwd.path());
        let initial_mtime = loader.project.as_ref().unwrap().mtime;
        rewrite_and_advance_mtime(&project_path, "v2");

        assert!(loader.check_staleness());
        let new_mtime = loader.project.as_ref().unwrap().mtime;
        assert_ne!(initial_mtime, new_mtime, "mtime must advance after re-read");
    }

    #[test]
    #[serial_test::serial]
    fn check_staleness_idempotent_after_a_change() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(home.path()));
        let project_path = cwd.path().join("NORN.md");
        write(&project_path, "v1");

        let mut loader = ContextLoader::load(cwd.path());
        rewrite_and_advance_mtime(&project_path, "v2");

        assert!(loader.check_staleness(), "first call after change is true");
        assert!(
            !loader.check_staleness(),
            "second call without further change is false"
        );
    }
}
