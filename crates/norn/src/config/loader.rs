//! Settings discovery and JSON parsing for the three on-disk layers.
//!
//! [`load_settings`] returns a [`LoadedSettings`] holding three independent
//! [`NornSettings`] values — user, project, and local — without merging them.
//! Merging is owned by [`crate::config::merge`]; this module only finds the
//! files, parses them, and reports typed errors.
//!
//! Missing files are not errors: per `DESIGN.md` D11 and constraint CO7, no
//! directory or file is auto-created on read. A non-existent file simply
//! resolves to [`NornSettings::default`] for that layer.
//!
//! Top-level keys that are not recognised by [`NornSettings`] are silently
//! dropped by serde during typed deserialisation. To preserve forward
//! compatibility (`DESIGN.md` D2; brief R7), the loader inspects the parsed
//! [`serde_json::Value`] *before* typed conversion and emits a
//! `tracing::warn!` for every unknown top-level key.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::paths;
use crate::config::types::NornSettings;
use crate::error::ConfigError;

/// The thirteen documented top-level settings keys. Anything outside this
/// set is reported via `tracing::warn!` so future-version settings files
/// remain readable on older binaries.
const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
    "model",
    "provider",
    "agent",
    "retry",
    "permissions",
    "hooks",
    "tools",
    "mcp_servers",
    "skills",
    "context",
    "session",
    "tui",
    "env",
];

/// Three independent settings layers as loaded from disk.
///
/// Each field carries the raw parsed [`NornSettings`] for that layer —
/// merging happens in [`crate::config::merge::merge_settings`], not here.
/// Missing files produce [`NornSettings::default`] (all-`None`) for the
/// corresponding layer.
#[derive(Clone, Debug, Default)]
pub struct LoadedSettings {
    /// User-level layer — `~/.norn/settings.json` (or `$NORN_HOME/settings.json`).
    pub user: NornSettings,
    /// Project-level layer — `<cwd>/.norn/settings.json`. Intended for
    /// check-in to source control.
    pub project: NornSettings,
    /// Local-override layer — `<cwd>/.norn/settings.local.json`. Intended
    /// to be `.gitignore`-d for personal overrides.
    pub local: NornSettings,
}

/// Discover, parse, and return the three settings layers for `cwd`.
///
/// User layer is resolved via [`paths::settings_file`]. Project and local
/// paths are derived directly from `cwd`. Missing files are tolerated;
/// malformed JSON and other I/O failures produce typed
/// [`ConfigError::InvalidConfig`].
///
/// # Errors
///
/// - The settings file exists but is not valid JSON.
/// - An I/O error other than `NotFound` occurs while reading any layer.
pub fn load_settings(cwd: &Path) -> Result<LoadedSettings, ConfigError> {
    let user_path = paths::settings_file();
    let project_path = cwd.join(".norn").join("settings.json");
    let local_path = cwd.join(".norn").join("settings.local.json");

    let user = match user_path {
        Some(p) => load_one_layer(&p)?,
        None => NornSettings::default(),
    };
    let project = load_one_layer(&project_path)?;
    let local = load_one_layer(&local_path)?;

    Ok(LoadedSettings {
        user,
        project,
        local,
    })
}

/// Read and parse a single settings file, tolerating absence.
///
/// Returns [`NornSettings::default`] when the file is missing. Returns a
/// typed [`ConfigError`] for malformed JSON or any other I/O failure.
fn load_one_layer(path: &Path) -> Result<NornSettings, ConfigError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NornSettings::default());
        }
        Err(err) => {
            return Err(ConfigError::InvalidConfig {
                reason: format!("failed to read {}: {err}", path.display()),
            });
        }
    };
    parse_layer(path, &contents)
}

/// Parse a single layer's JSON string, warning on unknown top-level keys
/// before producing the typed view.
fn parse_layer(path: &Path, contents: &str) -> Result<NornSettings, ConfigError> {
    let value: Value =
        serde_json::from_str(contents).map_err(|err| ConfigError::InvalidConfig {
            reason: format!("failed to parse {}: {err}", path.display()),
        })?;
    warn_on_unknown_keys(path, &value);
    serde_json::from_value::<NornSettings>(value).map_err(|err| ConfigError::InvalidConfig {
        reason: format!("failed to parse {}: {err}", path.display()),
    })
}

/// Emit a `tracing::warn!` for each top-level key in `value` that does not
/// appear in [`KNOWN_TOP_LEVEL_KEYS`]. Silently ignores non-object roots —
/// serde will reject them with a descriptive error at the typed-conversion
/// step.
fn warn_on_unknown_keys(path: &Path, value: &Value) {
    let Some(obj) = value.as_object() else {
        return;
    };
    for key in obj.keys() {
        if !KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()) {
            tracing::warn!(
                path = %path.display(),
                key = %key,
                "unknown top-level key in settings; ignoring",
            );
        }
    }
}

/// Convenience: the project-layer path derived from a working directory.
///
/// Exposed so downstream tooling can probe the conventional location
/// without re-deriving the join.
#[must_use]
pub fn project_settings_path(cwd: &Path) -> PathBuf {
    cwd.join(".norn").join("settings.json")
}

/// Convenience: the local-override path derived from a working directory.
#[must_use]
pub fn local_settings_path(cwd: &Path) -> PathBuf {
    cwd.join(".norn").join("settings.local.json")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unsafe_code,
    clippy::uninlined_format_args
)]
mod tests {
    use super::*;

    /// Guard that swaps `NORN_HOME` for the duration of a test and
    /// restores the prior value on drop. Mirrors the helper in
    /// `paths.rs` so loader tests can isolate user-layer resolution.
    struct NornHomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl NornHomeGuard {
        fn set(value: Option<&std::path::Path>) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            match value {
                Some(path) => unsafe { std::env::set_var("NORN_HOME", path) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
            Self { prior }
        }
    }

    impl Drop for NornHomeGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(val) => unsafe { std::env::set_var("NORN_HOME", val) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn missing_files_produce_default_settings() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let loaded = load_settings(cwd.path()).expect("load must succeed when all files missing");
        // All three layers default to all-None.
        assert!(loaded.user.model.is_none());
        assert!(loaded.user.agent.is_none());
        assert!(loaded.project.model.is_none());
        assert!(loaded.project.agent.is_none());
        assert!(loaded.local.model.is_none());
        assert!(loaded.local.agent.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn project_settings_path_helper_is_dotnorn_settings_json() {
        let cwd = tempfile::tempdir().unwrap();
        let p = project_settings_path(cwd.path());
        assert_eq!(p, cwd.path().join(".norn").join("settings.json"));
    }

    #[test]
    #[serial_test::serial]
    fn local_settings_path_helper_is_dotnorn_settings_local_json() {
        let cwd = tempfile::tempdir().unwrap();
        let p = local_settings_path(cwd.path());
        assert_eq!(p, cwd.path().join(".norn").join("settings.local.json"));
    }

    #[test]
    #[serial_test::serial]
    fn loads_project_settings_when_present() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(
            norn_dir.join("settings.json"),
            r#"{"model":"gpt-5.5","agent":{"max_turns":7}}"#,
        )
        .unwrap();

        let loaded = load_settings(cwd.path()).unwrap();
        assert_eq!(loaded.project.model.as_deref(), Some("gpt-5.5"));
        let agent = loaded.project.agent.unwrap();
        assert_eq!(agent.max_turns, Some(7));
        assert!(loaded.user.model.is_none());
        assert!(loaded.local.model.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn loads_local_settings_when_present() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(
            norn_dir.join("settings.local.json"),
            r#"{"model":"local-model"}"#,
        )
        .unwrap();

        let loaded = load_settings(cwd.path()).unwrap();
        assert_eq!(loaded.local.model.as_deref(), Some("local-model"));
        assert!(loaded.project.model.is_none());
        assert!(loaded.user.model.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn loads_user_settings_when_present() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        std::fs::write(
            user_home.path().join("settings.json"),
            r#"{"model":"user-model"}"#,
        )
        .unwrap();

        let loaded = load_settings(cwd.path()).unwrap();
        assert_eq!(loaded.user.model.as_deref(), Some("user-model"));
        assert!(loaded.project.model.is_none());
        assert!(loaded.local.model.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn malformed_json_produces_descriptive_error() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        let bad_path = norn_dir.join("settings.json");
        std::fs::write(&bad_path, "{ this is not json }").unwrap();

        let err = load_settings(cwd.path()).expect_err("malformed JSON must error");
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant, got {err:?}");
        };
        // Reason includes the file path and serde's parse description.
        assert!(
            reason.contains(&bad_path.display().to_string()),
            "reason missing file path: {reason}",
        );
        // serde_json's error message includes the word "expected" or "key" for
        // most malformed inputs — assert the file-path component and that the
        // reason isn't trivially short (real parser detail attached).
        assert!(reason.len() > bad_path.display().to_string().len() + 4);
    }

    #[test]
    #[serial_test::serial]
    fn unknown_top_level_key_emits_warn_and_loads_known_keys() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        // `mystery_field` is not in KNOWN_TOP_LEVEL_KEYS; serde drops it
        // silently. We assert that the typed view still loads, and the
        // known field is preserved.
        std::fs::write(
            norn_dir.join("settings.json"),
            r#"{"model":"gpt-5.5","mystery_field":{"x":1}}"#,
        )
        .unwrap();

        let loaded = load_settings(cwd.path()).unwrap();
        assert_eq!(loaded.project.model.as_deref(), Some("gpt-5.5"));
    }

    #[test]
    #[serial_test::serial]
    fn user_project_and_local_all_loaded_when_present() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        std::fs::write(
            user_home.path().join("settings.json"),
            r#"{"model":"user"}"#,
        )
        .unwrap();
        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(norn_dir.join("settings.json"), r#"{"model":"project"}"#).unwrap();
        std::fs::write(norn_dir.join("settings.local.json"), r#"{"model":"local"}"#).unwrap();

        let loaded = load_settings(cwd.path()).unwrap();
        assert_eq!(loaded.user.model.as_deref(), Some("user"));
        assert_eq!(loaded.project.model.as_deref(), Some("project"));
        assert_eq!(loaded.local.model.as_deref(), Some("local"));
    }

    #[test]
    #[serial_test::serial]
    fn empty_object_file_yields_default() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(norn_dir.join("settings.json"), "{}").unwrap();

        let loaded = load_settings(cwd.path()).unwrap();
        assert!(loaded.project.model.is_none());
        assert!(loaded.project.provider.is_none());
    }
}
