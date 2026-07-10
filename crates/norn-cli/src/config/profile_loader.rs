//! Profile resolution for the Norn CLI (NC-004 R1, NA-002 R4).
//!
//! Translates a `--profile` argument into a fully-loaded [`Profile`]:
//!
//! - A value containing `/` or `.` is treated as a filesystem path and
//!   loaded directly via [`Profile::from_file`].
//! - A bare name is delegated to [`norn::profile::resolve_workspace_profile`]
//!   with a
//!   workspace-aware resolution ordered workspace `.norn/profiles/` →
//!   workspace `.meridian/profiles/` → the `NORN_HOME`-aware user tier.
//!   The libnorn scanner prefers `.md` over `.toml` over `.json` within
//!   each directory and first-directory wins across them. Workspace profiles
//!   may not declare automatic prompt commands; user profiles and explicit
//!   profile paths retain that trusted capability.
//! - When the caller passes [`None`], a minimal default profile is built
//!   with the generated catalog default model (`gpt-5.6-sol`) and a default
//!   system instruction.
//!
//! Errors propagate as [`BuildError`] so the entry point can map them onto
//! the CLI argument-error exit code (`2`) per `DESIGN.md` CO5.

use std::path::Path;

use norn::profile::{self, Profile};

use crate::cli::BuildError;

/// Loaded CLI profile plus whether its file came from the working directory.
pub struct ResolvedCliProfile {
    /// Parsed profile.
    pub profile: Profile,
    /// True only for a bare name resolved from a workspace profile directory.
    pub working_directory_controlled: bool,
}

/// Default model identifier used when no `--profile` is supplied and no
/// `--model` override has been applied yet.
pub const DEFAULT_MODEL: &str = norn::model_catalog::DEFAULT_MODEL;

/// Default system instruction used when no `--system-prompt` is supplied
/// and the profile has no `system_instructions`. The `OpenAI` Responses API
/// requires a non-empty instructions field.
pub const DEFAULT_SYSTEM_INSTRUCTION: &str = "You are a helpful assistant.";

/// Resolve the `--profile` argument into a loaded [`Profile`].
///
/// `spec` is the raw value of `--profile` (i.e. `cli.profile.as_deref()`).
/// When `None`, the function returns [`default_profile`]; otherwise it
/// dispatches on the presence of path separators or extensions and loads
/// the file from disk.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when a bare name cannot be located as
/// `.md`, `.toml`, or `.json` in any of the configured scan directories, or
/// when the underlying profile file cannot be read or parsed.
pub fn resolve_profile(spec: Option<&str>) -> Result<Profile, BuildError> {
    Ok(resolve_profile_with_origin(spec)?.profile)
}

/// Resolves a CLI profile while retaining whether a bare name came from the
/// working directory.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] for profile load, parse, capability, or
/// working-directory prompt-command violations.
pub fn resolve_profile_with_origin(spec: Option<&str>) -> Result<ResolvedCliProfile, BuildError> {
    let Some(value) = spec else {
        return Ok(ResolvedCliProfile {
            profile: default_profile(),
            working_directory_controlled: false,
        });
    };

    if looks_like_path(value) {
        return Ok(ResolvedCliProfile {
            profile: load_from_path(Path::new(value))?,
            working_directory_controlled: false,
        });
    }

    let cwd = std::env::current_dir()?;
    let resolved = profile::resolve_workspace_profile(value, &cwd)?;
    Ok(ResolvedCliProfile {
        profile: resolved.profile,
        working_directory_controlled: resolved.origin == profile::ProfileOrigin::WorkingDirectory,
    })
}

/// Construct a minimal default profile with a sensible model and system
/// instruction.
///
/// The model defaults to [`DEFAULT_MODEL`] and the system instruction to
/// [`DEFAULT_SYSTEM_INSTRUCTION`]. Later CLI overrides (`--model`,
/// `--system-prompt`) replace them before the profile reaches `from_profile`.
#[must_use]
pub fn default_profile() -> Profile {
    Profile {
        model: DEFAULT_MODEL.to_owned(),
        system_instructions: vec![DEFAULT_SYSTEM_INSTRUCTION.to_owned()],
        ..Profile::default()
    }
}

/// Convenience predicate matching the brief's "contains `/` or `.`"
/// dispatch rule.
fn looks_like_path(value: &str) -> bool {
    value.contains('/') || value.contains('.')
}

/// Load a profile from `path`, wrapping the underlying `ConfigError` as a
/// [`BuildError::Argument`] so the caller maps it to the argument-error
/// exit code per `DESIGN.md` CO5.
fn load_from_path(path: &Path) -> Result<Profile, BuildError> {
    Profile::from_file(path).map_err(|err| {
        BuildError::Argument(format!("failed to load profile {}: {err}", path.display()))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Set `NORN_HOME` to a temp directory for the duration of a test.
    struct TempNornHome {
        prior: Option<std::ffi::OsString>,
        _tempdir: tempfile::TempDir,
    }

    impl TempNornHome {
        fn new(tempdir: tempfile::TempDir) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with the `#[serial]` markers on every consumer;
            // no concurrent reader observes the mutated env.
            unsafe { std::env::set_var("NORN_HOME", tempdir.path()) };
            Self {
                prior,
                _tempdir: tempdir,
            }
        }
    }

    impl Drop for TempNornHome {
        fn drop(&mut self) {
            match &self.prior {
                Some(val) => unsafe { std::env::set_var("NORN_HOME", val) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    #[test]
    fn default_profile_uses_gpt_5_6_sol_catalog_default() {
        let profile = default_profile();
        assert_eq!(DEFAULT_MODEL, "gpt-5.6-sol");
        assert_eq!(profile.model, DEFAULT_MODEL);
        assert_eq!(
            profile.system_instructions,
            vec![DEFAULT_SYSTEM_INSTRUCTION]
        );
        assert!(profile.capabilities.is_empty());
        assert!(profile.tools.is_none());
    }

    #[test]
    fn no_profile_argument_returns_default() {
        let profile = resolve_profile(None).unwrap();
        assert_eq!(profile.model, DEFAULT_MODEL);
    }

    #[test]
    fn looks_like_path_detects_separators() {
        assert!(looks_like_path("./my-profile.toml"));
        assert!(looks_like_path("/tmp/profile.json"));
        assert!(looks_like_path("relative/path"));
        assert!(looks_like_path("profile.toml"));
        assert!(!looks_like_path("coding"));
        assert!(!looks_like_path("bare-name"));
    }

    #[test]
    fn explicit_path_loads_directly_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.toml");
        std::fs::write(
            &path,
            "name = \"explicit\"\nmodel = \"gpt-5\"\nsystem_instructions = []\n\
             [[prompt_commands]]\nname = \"trusted\"\ncommand = \"printf trusted\"\n",
        )
        .unwrap();
        let profile = resolve_profile(Some(path.to_str().unwrap())).unwrap();
        assert_eq!(profile.name, "explicit");
        assert_eq!(profile.model, "gpt-5");
        assert_eq!(profile.prompt_commands.len(), 1);
    }

    #[test]
    fn explicit_path_missing_returns_argument_error() {
        let err = resolve_profile(Some("./does-not-exist.toml")).unwrap_err();
        match err {
            BuildError::Argument(reason) => {
                assert!(reason.contains("./does-not-exist.toml"));
            }
            other @ BuildError::Auth(_) => panic!("expected Argument error, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn bare_name_resolves_toml_in_profiles_dir() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("coding.toml"),
            "name = \"coding\"\nmodel = \"gpt-5\"\n",
        )
        .unwrap();
        let _guard = TempNornHome::new(dir);

        let profile = resolve_profile(Some("coding")).unwrap();
        assert_eq!(profile.name, "coding");
        assert_eq!(profile.model, "gpt-5");
    }

    #[test]
    #[serial]
    fn bare_name_resolves_json_when_toml_missing() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("review.json"),
            r#"{"name":"review","model":"gpt-5"}"#,
        )
        .unwrap();
        let _guard = TempNornHome::new(dir);

        let profile = resolve_profile(Some("review")).unwrap();
        assert_eq!(profile.name, "review");
    }

    #[test]
    #[serial]
    fn bare_name_resolves_markdown_via_libnorn() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("dev.md"),
            "---\nname: dev\nmodel: gpt-5\n---\nYou are a developer.\n",
        )
        .unwrap();
        let _guard = TempNornHome::new(dir);

        let profile = resolve_profile(Some("dev")).unwrap();
        assert_eq!(profile.name, "dev");
        assert_eq!(profile.model, "gpt-5");
        assert_eq!(
            profile.system_instructions,
            vec!["You are a developer.".to_owned()]
        );
    }

    #[test]
    #[serial]
    fn bare_name_missing_lists_searched_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        let user_profiles = dir.path().join("profiles");
        let _guard = TempNornHome::new(dir);

        let err = resolve_profile(Some("nonexistent")).unwrap_err();
        match err {
            BuildError::Argument(reason) => {
                assert!(reason.contains("nonexistent"), "reason: {reason}");
                assert!(
                    reason.contains(&user_profiles.display().to_string()),
                    "reason did not mention NORN_HOME profiles dir: {reason}"
                );
            }
            other @ BuildError::Auth(_) => panic!("expected Argument error, got {other:?}"),
        }
    }
}
