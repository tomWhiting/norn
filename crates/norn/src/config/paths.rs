//! Directory resolution for norn.
//!
//! All norn data lives under `~/.norn/`:
//!
//! - [`norn_dir`] — `~/.norn/` (root, honours `$NORN_HOME`).
//! - [`profiles_dir`] — `~/.norn/profiles/`.
//! - [`rules_dir`] — `~/.norn/rules/`.
//! - [`resolve_standard_session_data_dir`] — the checked
//!   `~/.norn/session-store/` runtime authority.
//! - [`skills_dir`] — `~/.norn/skills/`.
//! - [`settings_file`] — `~/.norn/settings.json`.
//!
//! General layout helpers return [`Option<PathBuf>`]. Security-sensitive
//! session resolvers return a typed [`ConfigError`] instead, so a missing or
//! invalid authority cannot silently become repository-relative storage.

use std::path::PathBuf;

use crate::error::ConfigError;

/// Root directory: `~/.norn/`.
pub(crate) const DEFAULT_NORN_DIRECTORY: &str = ".norn";

/// Subdirectory of `~/.norn/` containing named profile files.
const PROFILES_SUBDIR: &str = "profiles";

/// Subdirectory of `~/.norn/` containing user-level rule files.
///
/// Rule files in this directory are merged into the rules engine alongside
/// project-level rules from `{cwd}/.norn/rules/`. Project rules win on ID
/// collision (NX-002 R8 / DESIGN.md §D5).
const RULES_SUBDIR: &str = "rules";

/// Subdirectory for session JSONL files and index.
const SESSION_STORE_SUBDIR: &str = "session-store";

/// Legacy session namespace consumed only by the explicit offline migrator.
const LEGACY_SESSIONS_SUBDIR: &str = "sessions";

/// Subdirectory of `~/.norn/` containing user-level skill packages.
const SKILLS_SUBDIR: &str = "skills";

/// File name of the user-level settings document.
const SETTINGS_FILE: &str = "settings.json";

/// Environment variable to override the root directory (for testing/CI).
const NORN_HOME: &str = "NORN_HOME";

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

/// Resolve the norn root directory.
///
/// Uses `$NORN_HOME` when set and non-empty, otherwise `~/.norn/`.
/// Returns [`None`] only when neither source can be resolved (i.e. there
/// is no home directory and `$NORN_HOME` is unset/empty).
#[must_use]
pub fn norn_dir() -> Option<PathBuf> {
    if let Some(override_dir) = std::env::var_os(NORN_HOME)
        && !override_dir.is_empty()
    {
        let path = PathBuf::from(override_dir);
        if path.is_absolute() {
            return Some(path);
        }
        tracing::warn!("ignoring relative NORN_HOME; the override must be absolute");
    }
    trusted_home_dir().map(|home| home.join(DEFAULT_NORN_DIRECTORY))
}

/// Resolve the trusted Norn root used by session persistence.
///
/// Unlike [`norn_dir`], this checked resolver never ignores an invalid
/// authority and falls through to another one. A non-empty `NORN_HOME` must
/// be absolute; otherwise the operating-system home must exist and be
/// absolute.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidConfig`] when the selected authority is
/// relative or no trusted user root can be resolved.
pub fn resolve_session_norn_root() -> Result<PathBuf, ConfigError> {
    resolve_session_norn_root_from(std::env::var_os(NORN_HOME), dirs::home_dir)
}

fn resolve_session_norn_root_from(
    override_dir: Option<std::ffi::OsString>,
    home_dir: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf, ConfigError> {
    if let Some(value) = override_dir
        && !value.is_empty()
    {
        let root = PathBuf::from(value);
        if root.is_absolute() {
            return Ok(root);
        }
        return Err(ConfigError::InvalidConfig {
            reason: "NORN_HOME must be an absolute path so a working directory cannot become the trusted session-storage tier"
                .to_owned(),
        });
    }

    let home = home_dir().ok_or_else(|| ConfigError::InvalidConfig {
        reason: "session persistence requires an absolute NORN_HOME or user home directory"
            .to_owned(),
    })?;
    if !home.is_absolute() {
        return Err(ConfigError::InvalidConfig {
            reason: "the user home directory must be absolute so a working directory cannot become the trusted session-storage tier"
                .to_owned(),
        });
    }
    Ok(home.join(DEFAULT_NORN_DIRECTORY))
}

/// Returns the operating-system home directory only when it is absolute.
///
/// A relative home would make trusted user configuration and credentials
/// depend on the process working directory.
#[must_use]
pub fn trusted_home_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    if home.is_absolute() {
        Some(home)
    } else {
        tracing::warn!("ignoring relative home directory for trusted path resolution");
        None
    }
}

/// Rejects a non-empty relative `NORN_HOME` before trust-tier loading.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidConfig`] when `NORN_HOME` is relative. The
/// configured value is deliberately omitted from the diagnostic because it
/// may be controlled by a launch wrapper.
pub fn validate_norn_home() -> Result<(), ConfigError> {
    if let Some(value) = std::env::var_os(NORN_HOME)
        && !value.is_empty()
        && !PathBuf::from(value).is_absolute()
    {
        return Err(ConfigError::InvalidConfig {
            reason: "NORN_HOME must be an absolute path so a working directory cannot become the trusted user configuration tier"
                .to_owned(),
        });
    }
    Ok(())
}

/// Rejects a relative operating-system home before trusted-tier loading.
pub(crate) fn validate_user_home() -> Result<(), ConfigError> {
    if dirs::home_dir().is_some_and(|home| !home.is_absolute()) {
        return Err(ConfigError::InvalidConfig {
            reason: "the user home directory must be absolute so a working directory cannot become a trusted configuration tier"
                .to_owned(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Profiles directory
// ---------------------------------------------------------------------------

/// Resolve the directory containing named profile files: `~/.norn/profiles/`.
///
/// Sits directly under [`norn_dir`] alongside the active `session-store/`
/// and legacy `sessions/` namespaces. Profiles remain outside any `config/`
/// subdirectory so the user-level layout stays flat.
#[must_use]
pub fn profiles_dir() -> Option<PathBuf> {
    norn_dir().map(|d| d.join(PROFILES_SUBDIR))
}

/// Resolve the user-level rules directory: `~/.norn/rules/`.
///
/// Rule files in this directory are merged into the rules engine
/// alongside project-level rules from `{cwd}/.norn/rules/`. Project
/// rules win on ID collision (DESIGN.md §D5). Returns [`None`] when
/// neither `$NORN_HOME` nor [`dirs::home_dir`] resolves.
#[must_use]
pub fn rules_dir() -> Option<PathBuf> {
    norn_dir().map(|d| d.join(RULES_SUBDIR))
}

// ---------------------------------------------------------------------------
// Session data directory
// ---------------------------------------------------------------------------

/// Resolve the active strict session-store directory: `~/.norn/session-store/`.
///
/// This is the standard runtime front door for library embedders. It resolves
/// a trusted absolute root and, when the legacy `~/.norn/sessions/` namespace
/// still exists, verifies the fixed-size cutover proof inside the active
/// store. The guard observes legacy-path metadata only; it never opens or
/// decodes legacy session content during normal startup.
///
/// [`crate::session::SessionManager::new`] remains available for deliberately
/// custom, embedder-owned stores. Do not combine it with a path assembled from
/// [`norn_dir`] when the standard user store is intended.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidConfig`] when no trusted root exists, a
/// session namespace cannot be inspected, or legacy data exists without a
/// complete bounded cutover proof.
pub fn resolve_standard_session_data_dir() -> Result<PathBuf, ConfigError> {
    let root = resolve_session_norn_root()?;
    resolve_standard_session_data_dir_at(&root)
}

/// Resolve the legacy session directory: `~/.norn/sessions/`.
///
/// Normal runtime code must not read this namespace. It exists only as the
/// immutable input to the explicit offline session migration command.
#[must_use]
pub fn legacy_session_data_dir() -> Option<PathBuf> {
    norn_dir().map(|d| d.join(LEGACY_SESSIONS_SUBDIR))
}

fn resolve_standard_session_data_dir_at(root: &std::path::Path) -> Result<PathBuf, ConfigError> {
    let active = root.join(SESSION_STORE_SUBDIR);
    let legacy = root.join(LEGACY_SESSIONS_SUBDIR);
    if path_entry_exists(&legacy)?
        && let Err(error) = crate::session::verify_legacy_session_cutover(root)
    {
        return Err(ConfigError::InvalidConfig {
            reason: format!(
                "legacy sessions exist without a complete verified migration; run 'norn session migrate' or inspect the session namespaces before continuing: {error}"
            ),
        });
    }
    Ok(active)
}

fn path_entry_exists(path: &std::path::Path) -> Result<bool, ConfigError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(ConfigError::InvalidConfig {
            reason: format!("could not inspect the session storage namespace: {error}"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Skills directory
// ---------------------------------------------------------------------------

/// Resolve the user-level skills directory: `~/.norn/skills/`.
///
/// Returns [`None`] when neither `$NORN_HOME` nor [`dirs::home_dir`]
/// resolves. Callers that need to scan project-local skill trees
/// (`.norn/skills/`, `.agents/skills/`, `.claude/skills/`) compose those
/// from a working directory in the CLI layer — this helper covers the
/// user-level tier only.
#[must_use]
pub fn skills_dir() -> Option<PathBuf> {
    norn_dir().map(|d| d.join(SKILLS_SUBDIR))
}

// ---------------------------------------------------------------------------
// Settings file
// ---------------------------------------------------------------------------

/// Resolve the user-level settings document path: `~/.norn/settings.json`.
///
/// Project-level (`./.norn/settings.json`) and local-override
/// (`./.norn/settings.local.json`) files are resolved from the working
/// directory at load time, not from this helper.
#[must_use]
pub fn settings_file() -> Option<PathBuf> {
    norn_dir().map(|d| d.join(SETTINGS_FILE))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use super::*;

    /// Guard that swaps `NORN_HOME` for the duration of a test and
    /// restores the prior value on drop. Tests are serialised via
    /// `#[serial_test::serial]`.
    struct NornHomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl NornHomeGuard {
        fn set(value: Option<&std::path::Path>) -> Self {
            let prior = std::env::var_os(NORN_HOME);
            // SAFETY: paired with `#[serial]` on every consumer; no
            // concurrent reader observes the mutated env.
            match value {
                Some(path) => unsafe { std::env::set_var(NORN_HOME, path) },
                None => unsafe { std::env::remove_var(NORN_HOME) },
            }
            Self { prior }
        }

        fn set_empty() -> Self {
            let prior = std::env::var_os(NORN_HOME);
            // SAFETY: see [`Self::set`].
            unsafe { std::env::set_var(NORN_HOME, "") };
            Self { prior }
        }
    }

    impl Drop for NornHomeGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(val) => unsafe { std::env::set_var(NORN_HOME, val) },
                None => unsafe { std::env::remove_var(NORN_HOME) },
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn norn_dir_falls_back_to_home_when_env_unset() {
        let _guard = NornHomeGuard::set(None);
        let Some(dir) = norn_dir() else {
            return;
        };
        let home = dirs::home_dir().expect("home_dir must resolve");
        assert_eq!(dir, home.join(".norn"));
    }

    #[test]
    #[serial_test::serial]
    fn norn_dir_honours_norn_home_when_set_and_nonempty() {
        let tempdir = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(tempdir.path()));
        let dir = norn_dir().expect("NORN_HOME override must produce Some");
        assert_eq!(dir, tempdir.path());
    }

    #[test]
    #[serial_test::serial]
    fn norn_dir_skips_norn_home_when_empty() {
        let _guard = NornHomeGuard::set_empty();
        let Some(dir) = norn_dir() else {
            return;
        };
        // Empty NORN_HOME must NOT be used as the directory — we should
        // see the home_dir fallback path instead.
        let home = dirs::home_dir().expect("home_dir must resolve");
        assert_eq!(dir, home.join(".norn"));
    }

    #[test]
    #[serial_test::serial]
    fn profiles_dir_appends_profiles_subdir() {
        let tempdir = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(tempdir.path()));
        let dir = profiles_dir().expect("profiles_dir must produce Some");
        assert_eq!(dir, tempdir.path().join("profiles"));
    }

    #[test]
    #[serial_test::serial]
    fn legacy_session_data_dir_appends_legacy_subdir_and_is_some() {
        let tempdir = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(tempdir.path()));
        assert_eq!(
            legacy_session_data_dir(),
            Some(tempdir.path().join("sessions"))
        );
    }

    #[test]
    #[serial_test::serial]
    fn skills_dir_appends_skills_subdir() {
        let tempdir = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(tempdir.path()));
        let dir = skills_dir().expect("skills_dir must produce Some");
        assert_eq!(dir, tempdir.path().join("skills"));
    }

    #[test]
    #[serial_test::serial]
    fn settings_file_appends_settings_json() {
        let tempdir = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(tempdir.path()));
        let path = settings_file().expect("settings_file must produce Some");
        assert_eq!(path, tempdir.path().join("settings.json"));
        assert!(path.ends_with("settings.json"));
    }

    #[test]
    #[serial_test::serial]
    fn rules_dir_appends_rules_subdir() {
        let tempdir = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(tempdir.path()));
        let dir = rules_dir().expect("rules_dir must produce Some");
        assert_eq!(dir, tempdir.path().join("rules"));
    }
}

#[cfg(test)]
#[path = "paths_session_tests.rs"]
mod standard_session_tests;
