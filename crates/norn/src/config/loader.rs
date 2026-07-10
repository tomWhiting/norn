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
//! Keys that are not recognised by [`NornSettings`] — at ANY nesting depth
//! — are silently dropped by serde during typed deserialisation. To
//! preserve forward compatibility (`DESIGN.md` D2; brief R7) while still
//! surfacing typos, the loader deserialises through `serde_ignored` and
//! emits a `tracing::warn!` naming the full dotted path of every ignored
//! key (e.g. `agent.max_turnz`), so a nested typo never silently
//! deserialises to a default.
//!
//! Known limitation: sections deserialised via `#[serde(flatten)]`
//! (`provider_profiles.<name>.*`) buffer their content, so unknown keys
//! inside a flattened section are not reported by `serde_ignored`.

use std::path::{Path, PathBuf};

use crate::config::paths;
use crate::config::types::NornSettings;
use crate::error::ConfigError;
use crate::util::read_workspace_text_file;

/// Three independent settings layers as loaded from disk.
///
/// Each field carries the raw parsed [`NornSettings`] for that layer —
/// merging happens in [`crate::config::merge::merge_settings`], not here.
/// Missing files produce [`NornSettings::default`] (all-`None`) for the
/// corresponding layer.
#[derive(Clone, Debug, Default)]
pub(crate) struct LoadedSettings {
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
/// [`ConfigError::InvalidConfig`]. The layers are intentionally raw; runtime
/// assemblers must call
/// [`super::validate_working_directory_authority`] before merging them.
///
/// # Errors
///
/// - The settings file exists but is not valid JSON.
/// - An I/O error other than `NotFound` occurs while reading any layer.
#[cfg(test)]
pub(crate) fn load_settings(cwd: &Path) -> Result<LoadedSettings, ConfigError> {
    let cwd = cwd
        .canonicalize()
        .map_err(|error| ConfigError::InvalidConfig {
            reason: format!("failed to resolve the working-directory trust root: {error}"),
        })?;
    load_settings_at_launch_root(&cwd)
}

/// Loads settings from an already-canonical immutable launch root.
pub(crate) fn load_settings_at_launch_root(cwd: &Path) -> Result<LoadedSettings, ConfigError> {
    paths::validate_norn_home()?;
    paths::validate_user_home()?;
    let user_path = paths::settings_file();
    let project_path = cwd.join(".norn").join("settings.json");
    let local_path = cwd.join(".norn").join("settings.local.json");

    let user = match user_path {
        Some(p) => load_one_layer(&p)?,
        None => NornSettings::default(),
    };
    let project = load_workspace_layer(cwd, Path::new(".norn/settings.json"), &project_path)?;
    let local = load_workspace_layer(cwd, Path::new(".norn/settings.local.json"), &local_path)?;

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

fn load_workspace_layer(
    root: &Path,
    relative: &Path,
    display_path: &Path,
) -> Result<NornSettings, ConfigError> {
    let contents = match read_workspace_text_file(root, relative) {
        Ok(loaded) => loaded.content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NornSettings::default());
        }
        Err(error) => {
            return Err(ConfigError::InvalidConfig {
                reason: format!(
                    "refused working-directory settings file {}: {error}",
                    display_path.display(),
                ),
            });
        }
    };
    parse_layer(display_path, &contents)
}

/// Parse a single layer's JSON string, warning on every ignored key —
/// top-level or nested — before returning the typed view.
fn parse_layer(path: &Path, contents: &str) -> Result<NornSettings, ConfigError> {
    let (settings, unknown_paths) =
        parse_settings_with_unknown_paths(contents).map_err(|err| ConfigError::InvalidConfig {
            reason: format!("failed to parse {}: {err}", path.display()),
        })?;
    for key_path in &unknown_paths {
        tracing::warn!(
            path = %path.display(),
            key = %key_path,
            "unknown key in settings; ignoring",
        );
    }
    Ok(settings)
}

/// Deserialise a settings document, collecting the full dotted path of
/// every key the typed schema ignores (top-level and nested).
///
/// Pure so tests can assert on the exact reported paths without
/// capturing tracing output. Flattened sections
/// (`provider_profiles.<name>`) buffer their content and therefore
/// cannot report ignored keys — see the module docs.
///
/// # Errors
///
/// Propagates the `serde_json` error for malformed JSON or a document
/// that violates the typed schema (e.g. a hook entry missing its
/// required `timeout`).
pub(crate) fn parse_settings_with_unknown_paths(
    contents: &str,
) -> Result<(NornSettings, Vec<String>), serde_json::Error> {
    let mut unknown: Vec<String> = Vec::new();
    let mut de = serde_json::Deserializer::from_str(contents);
    let settings: NornSettings = serde_ignored::deserialize(&mut de, |ignored| {
        // serde_ignored renders each `Option` hop as a `?` segment
        // (`agent.?.max_turnz`); collapse those so the reported path
        // matches the JSON the operator actually wrote.
        unknown.push(ignored.to_string().replace(".?.", "."));
    })?;
    de.end()?;
    Ok((settings, unknown))
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

    struct HomeGuard {
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

    impl HomeGuard {
        fn set_relative() -> Self {
            let prior = std::env::var_os("HOME");
            unsafe { std::env::set_var("HOME", "repository-user-home") };
            Self { prior }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => unsafe { std::env::set_var("HOME", value) },
                None => unsafe { std::env::remove_var("HOME") },
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
    fn relative_norn_home_is_rejected_before_user_tier_loading()
    -> Result<(), Box<dyn std::error::Error>> {
        let cwd = tempfile::tempdir()?;
        let relative_home = cwd.path().join("repository-user-tier");
        std::fs::create_dir(&relative_home)?;
        std::fs::write(
            relative_home.join("settings.json"),
            r#"{"model":"sentinel-repository-user-model"}"#,
        )?;
        let norn_home_guard =
            NornHomeGuard::set(Some(std::path::Path::new("repository-user-tier")));

        let result = load_settings(cwd.path());
        let error = result
            .err()
            .ok_or_else(|| std::io::Error::other("relative NORN_HOME became user authority"))?
            .to_string();

        assert!(error.contains("NORN_HOME must be an absolute path"));
        assert!(!error.contains("sentinel-repository-user-model"));
        drop(norn_home_guard);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn relative_home_is_rejected_before_user_tier_loading() -> Result<(), Box<dyn std::error::Error>>
    {
        let cwd = tempfile::tempdir()?;
        let _norn_home = NornHomeGuard::set(None);
        let _home = HomeGuard::set_relative();

        let result = load_settings(cwd.path());
        let error = result
            .err()
            .ok_or_else(|| std::io::Error::other("relative HOME became user authority"))?
            .to_string();

        assert!(error.contains("home directory must be absolute"));
        Ok(())
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

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn working_directory_settings_refuse_file_and_directory_symlinks()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let user_home = tempfile::tempdir()?;
        let norn_home_guard = NornHomeGuard::set(Some(user_home.path()));
        let outside = tempfile::tempdir()?;
        std::fs::write(
            outside.path().join("settings.json"),
            r#"{"model":"sentinel-private-settings"}"#,
        )?;

        let file_link_cwd = tempfile::tempdir()?;
        std::fs::create_dir(file_link_cwd.path().join(".norn"))?;
        symlink(
            outside.path().join("settings.json"),
            file_link_cwd.path().join(".norn/settings.json"),
        )?;
        let file_result = load_settings(file_link_cwd.path());
        let file_error = file_result
            .err()
            .ok_or_else(|| std::io::Error::other("project settings file symlink was accepted"))?
            .to_string();
        assert!(!file_error.contains("sentinel-private-settings"));

        let directory_link_cwd = tempfile::tempdir()?;
        symlink(outside.path(), directory_link_cwd.path().join(".norn"))?;
        let directory_result = load_settings(directory_link_cwd.path());
        let directory_error = directory_result
            .err()
            .ok_or_else(|| {
                std::io::Error::other("project settings directory symlink was accepted")
            })?
            .to_string();
        assert!(!directory_error.contains("sentinel-private-settings"));

        drop(norn_home_guard);
        Ok(())
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
    fn unknown_top_level_key_is_reported_and_known_keys_still_load() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(
            norn_dir.join("settings.json"),
            r#"{"model":"gpt-5.5","mystery_field":{"x":1}}"#,
        )
        .unwrap();

        // The typed view still loads through the full path...
        let loaded = load_settings(cwd.path()).unwrap();
        assert_eq!(loaded.project.model.as_deref(), Some("gpt-5.5"));

        // ...and the pure detector reports the unknown key by path.
        let (_, unknown) =
            parse_settings_with_unknown_paths(r#"{"model":"gpt-5.5","mystery_field":{"x":1}}"#)
                .unwrap();
        assert_eq!(unknown, vec!["mystery_field".to_owned()]);
    }

    /// `model_aliases` and `provider_profiles` are documented top-level
    /// keys — they must not produce false unknown-key reports (the old
    /// hand-maintained key list omitted them).
    #[test]
    fn documented_top_level_keys_produce_no_unknown_reports() {
        let json = r#"{
            "model": "gpt-5.5",
            "model_aliases": {"55": "gpt-5.5"},
            "provider_profiles": {
                "lmstudio": {
                    "api_shape": "openai_chat_completions",
                    "base_url": "http://localhost:1234/v1"
                }
            }
        }"#;
        let (settings, unknown) = parse_settings_with_unknown_paths(json).unwrap();
        assert!(
            unknown.is_empty(),
            "documented keys must not be reported as unknown: {unknown:?}"
        );
        assert!(settings.model_aliases.is_some());
        assert!(settings.provider_profiles.is_some());
    }

    /// A nested typo (`agent.max_turnz`) used to deserialise silently to
    /// the default; the detector now reports it with a path-qualified
    /// name.
    #[test]
    fn nested_typo_is_reported_with_dotted_path() {
        let json = r#"{"agent":{"max_turnz":9,"step_timeout":"30s"}}"#;
        let (settings, unknown) = parse_settings_with_unknown_paths(json).unwrap();
        assert_eq!(unknown, vec!["agent.max_turnz".to_owned()]);
        let agent = settings.agent.expect("agent section loads");
        assert_eq!(agent.step_timeout.as_deref(), Some("30s"));
        assert!(
            agent.max_turns.is_none(),
            "the typo'd key must not populate the real field"
        );
    }

    /// `tools.skill.shell_execution` is a documented typed key — the
    /// nested unknown-key detector must not flag it, while a typo'd
    /// sibling under the same section is still reported by path.
    #[test]
    fn tools_skill_shell_execution_is_a_known_key() {
        let json = r#"{"tools":{"skill":{"shell_execution":false}}}"#;
        let (settings, unknown) = parse_settings_with_unknown_paths(json).unwrap();
        assert!(
            unknown.is_empty(),
            "tools.skill.shell_execution must not be reported as unknown: {unknown:?}"
        );
        let skill = settings
            .tools
            .expect("tools section loads")
            .skill
            .expect("skill section loads");
        assert_eq!(skill.shell_execution, Some(false));

        let (_, unknown) =
            parse_settings_with_unknown_paths(r#"{"tools":{"skill":{"shell_exec":false}}}"#)
                .unwrap();
        assert_eq!(unknown, vec!["tools.skill.shell_exec".to_owned()]);
    }

    /// Deeply nested unknown keys are reported with their full path,
    /// including array indices.
    #[test]
    fn hook_entry_typo_is_reported_with_indexed_path() {
        let json = r#"{"hooks":{"pre_tool":[
            {"matcher":"Write","command":"lint.sh","timeout":5,"timout":9}
        ]}}"#;
        let (_, unknown) = parse_settings_with_unknown_paths(json).unwrap();
        assert_eq!(unknown, vec!["hooks.pre_tool.0.timout".to_owned()]);
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
