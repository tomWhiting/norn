//! Shared CLI resolution — the profile, settings, and provider selection
//! every driver (print, driven, TUI) resolves before handing off to
//! [`builder_from_cli`](crate::runtime::builder_from_cli).
//!
//! Provider construction is a CLI config surface, not library assembly:
//! the concrete [`Provider`](norn::provider::traits::Provider) is built
//! from the resolved model and overrides *before* the builder runs, then
//! passed into `builder_from_cli`. This module owns the resolution that
//! precedes that construction — model-alias and provider-profile
//! resolution, settings merge, CLI profile overrides, and reasoning-effort
//! validation — so the three drivers share one code path instead of
//! re-deriving it.

use std::time::Duration;

use norn::agent_loop::{
    effort_label, reasoning_effort_supported_for_model, unsupported_reasoning_effort_message,
};
use norn::config::NornSettings;
use norn::profile::Profile;
use norn::runtime_init::load_merged_settings;

use crate::cli::{BuildError, Cli, ProviderKind};
use crate::config::{
    AppliedOverrides, ConfigOverrides, ProviderConfigOverrides, apply_cli_profile_overrides,
    apply_settings_reasoning_to_profile, apply_working_dir, overlay_cli_provider_overrides,
    overlay_provider_profile_overrides, provider_overrides_from_settings,
    resolve_index_lock_deadline, resolve_model_selection, resolve_profile_with_origin,
    resolve_provider_selection,
};

/// The resolved CLI invocation state each driver needs to construct the
/// provider and the [`AgentBuilder`](norn::agent::AgentBuilder).
///
/// [`Self::profile`] carries the CLI model / tool / reasoning overrides
/// (produced by [`apply_cli_profile_overrides`], recorded in
/// [`Self::applied`]); [`Self::provider_kind`] and
/// [`Self::provider_overrides`] drive the concrete provider construction;
/// [`Self::model`] is the resolved model identifier (a copy of
/// `profile.model`, kept after the profile is moved into the builder).
pub struct ResolvedInvocation {
    /// The merged, validated settings both the provider construction and
    /// the builder's `load_runtime_base` consult.
    pub settings: NornSettings,
    /// The resolved profile with model / tool / reasoning overrides
    /// applied, ready to move into `builder_from_cli`.
    pub profile: Profile,
    /// The applied-overrides side channel (disallowed tools, unmatched
    /// tool flag names) `builder_from_cli` consumes.
    pub applied: AppliedOverrides,
    /// The selected provider backend.
    pub provider_kind: ProviderKind,
    /// The resolved provider-config overrides for the concrete provider
    /// construction (base URL, timeouts, retries, debug dump).
    pub provider_overrides: ProviderConfigOverrides,
    /// The resolved model identifier.
    pub model: String,
    /// The resolved root delegation depth for
    /// [`cli_coordination_envelope`](crate::runtime::cli_coordination_envelope):
    /// `-c delegation_depth` wins over the `[agent] delegation_depth`
    /// setting, which wins over the owner-ruled default of
    /// [`DEFAULT_DELEGATION_DEPTH`](crate::runtime::DEFAULT_DELEGATION_DEPTH)
    /// (`2`, DECISIONS §0.6(d)).
    pub delegation_depth: u32,
    /// The resolved session index-lock acquisition deadline
    /// ([`resolve_index_lock_deadline`]): `-c index_lock_deadline_ms`
    /// wins over `agent.index_lock_deadline_ms` from settings, which
    /// wins over the owner-ruled compiled default. Drivers apply it to
    /// every lock-taking [`SessionManager`](norn::session::SessionManager)
    /// they construct *outside* the `builder_from_cli` funnel (which
    /// resolves the same value itself) — e.g. the slash `/name` index
    /// rename and the TUI `/new` session rotation.
    pub index_lock_deadline: Duration,
}

/// Resolve a CLI invocation into the provider selection + profile the
/// drivers hand to [`builder_from_cli`](crate::runtime::builder_from_cli).
///
/// Applies `--working-dir` (mutating the process CWD, exactly as the
/// legacy `build_runtime` did as its first step), merges and validates the
/// settings tiers, resolves the profile (with the settings-model fallback
/// when neither `--profile` nor `-m` is given), layers the CLI profile
/// overrides, resolves the model alias, validates the reasoning effort
/// against the model, and resolves the provider selection + overrides.
///
/// # Errors
///
/// [`BuildError`] when the working directory cannot be applied, the
/// settings fail to load / validate, the profile or model cannot be
/// resolved, the reasoning effort is unsupported for the model, or the
/// provider selection / overrides fail to resolve.
pub fn resolve_invocation(cli: &Cli) -> Result<ResolvedInvocation, BuildError> {
    apply_working_dir(cli)?;

    let cwd = std::env::current_dir()?;
    let settings =
        load_merged_settings(&cwd).map_err(|error| BuildError::Argument(error.to_string()))?;

    let resolved_profile = resolve_profile_with_origin(cli.profile.as_deref())?;
    let profile_is_working_directory_controlled = resolved_profile.working_directory_controlled;
    let mut profile = resolved_profile.profile;
    if cli.profile.is_none()
        && cli.model.is_none()
        && let Some(model) = settings.model.as_deref()
    {
        model.clone_into(&mut profile.model);
    }
    apply_settings_reasoning_to_profile(&settings, &mut profile)?;
    let applied = apply_cli_profile_overrides(cli, &mut profile)?;
    let model_selection = resolve_model_selection(&profile.model, &settings)?;
    if profile_is_working_directory_controlled
        && cli.model.is_none()
        && (model_selection.provider_profile.is_some() || model_selection.api_shape.is_some())
    {
        return Err(BuildError::Argument(
            "working-directory profile models cannot select provider_profile or api_shape through a model alias; use an explicit --model selection or a user profile"
                .to_owned(),
        ));
    }
    profile.model.clone_from(&model_selection.model);
    validate_reasoning_effort_for_model(&profile)?;

    let mut config_overrides = ConfigOverrides::parse(&cli.config)?;
    if let Some(debug_api) = &cli.debug_api {
        config_overrides.debug_dump_dir = Some(resolve_debug_api_dir(debug_api)?);
    }

    let provider_selection = resolve_provider_selection(cli, &settings, &model_selection)?;
    let mut provider_overrides = provider_overrides_from_settings(&settings)?;
    if let Some(profile_name) = provider_selection.profile_name.as_deref() {
        let profile_overrides = settings
            .provider_profiles
            .as_ref()
            .and_then(|profiles| profiles.get(profile_name))
            .ok_or_else(|| {
                BuildError::Argument(format!(
                    "provider profile '{profile_name}' disappeared during runtime assembly",
                ))
            })?;
        overlay_provider_profile_overrides(
            &mut provider_overrides,
            profile_name,
            profile_overrides,
        )?;
    }
    overlay_cli_provider_overrides(&mut provider_overrides, &config_overrides);
    if provider_overrides
        .debug_dump_dir
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(BuildError::Argument(
            "debug dump directories must be absolute because dumps contain provider payloads"
                .to_owned(),
        ));
    }

    // Root delegation depth: `-c delegation_depth` wins over the `[agent]
    // delegation_depth` setting, which wins over the owner-ruled default
    // (DECISIONS §0.6(d)). The inherit-with-decrement and narrowing-only
    // invariants are untouched — this only seeds the root's own budget.
    let delegation_depth = config_overrides
        .delegation_depth
        .or_else(|| {
            settings
                .agent
                .as_ref()
                .and_then(|agent| agent.delegation_depth)
        })
        .unwrap_or(crate::runtime::DEFAULT_DELEGATION_DEPTH);

    // Resolved once here so every driver-side SessionManager outside the
    // `builder_from_cli` funnel (slash `/name` rename, TUI `/new`
    // rotation) applies the same bounded index-lock wait the funnel
    // itself applies — never the library's indefinite default.
    let index_lock_deadline = resolve_index_lock_deadline(&settings, &config_overrides)?;

    let model = profile.model.clone();
    Ok(ResolvedInvocation {
        settings,
        profile,
        applied,
        provider_kind: provider_selection.kind,
        provider_overrides,
        model,
        delegation_depth,
        index_lock_deadline,
    })
}

/// Reject a `--reasoning-effort` the resolved model does not support, so
/// the failure surfaces as an argument error instead of an opaque
/// provider rejection mid-run.
fn validate_reasoning_effort_for_model(profile: &Profile) -> Result<(), BuildError> {
    let Some(effort) = profile.reasoning_effort else {
        return Ok(());
    };
    if reasoning_effort_supported_for_model(&profile.model, effort) {
        return Ok(());
    }
    Err(BuildError::Argument(unsupported_reasoning_effort_message(
        &profile.model,
        effort_label(effort),
    )))
}

/// Resolve the `--debug-api` value into the JSONL dump directory: an
/// explicit path is used verbatim, an empty value defaults to
/// `~/.norn/debug`. Relative paths and an unavailable trusted home are
/// rejected instead of resolving sensitive dumps against the repository.
fn resolve_debug_api_dir(value: &str) -> Result<std::path::PathBuf, BuildError> {
    use std::path::PathBuf;
    if value.is_empty() {
        return crate::config::paths::norn_dir()
            .map(|root| root.join("debug"))
            .ok_or_else(|| {
                BuildError::Argument(
                    "--debug-api requires an absolute NORN_HOME or user home directory".to_owned(),
                )
            });
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(BuildError::Argument(
            "--debug-api paths must be absolute because dumps contain provider payloads".to_owned(),
        ));
    }
    Ok(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, unsafe_code)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use clap::Parser;
    use serial_test::serial;

    use super::*;

    struct IsolatedResolutionEnvironment {
        previous_dir: PathBuf,
        previous_norn_home: Option<OsString>,
        norn_home: tempfile::TempDir,
        working_dir: tempfile::TempDir,
    }

    impl IsolatedResolutionEnvironment {
        fn new() -> Self {
            let previous_dir = std::env::current_dir().unwrap();
            let previous_norn_home = std::env::var_os("NORN_HOME");
            let norn_home = tempfile::tempdir().unwrap();
            let working_dir = tempfile::tempdir().unwrap();

            // SAFETY: this test is serialised and the prior value is restored
            // by Drop before the temporary directory is removed.
            unsafe { std::env::set_var("NORN_HOME", norn_home.path()) };
            std::env::set_current_dir(working_dir.path()).unwrap();

            Self {
                previous_dir,
                previous_norn_home,
                norn_home,
                working_dir,
            }
        }

        fn norn_home(&self) -> &std::path::Path {
            self.norn_home.path()
        }

        fn working_dir(&self) -> &std::path::Path {
            self.working_dir.path()
        }
    }

    impl Drop for IsolatedResolutionEnvironment {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous_dir).unwrap();
            match &self.previous_norn_home {
                Some(value) => unsafe { std::env::set_var("NORN_HOME", value) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    #[test]
    #[serial]
    fn resolve_invocation_canonicalizes_cli_model_catalog_alias() {
        let _environment = IsolatedResolutionEnvironment::new();
        let cli = Cli::try_parse_from(["norn", "--model", "sol"]).unwrap();
        let resolved = resolve_invocation(&cli).unwrap();

        assert_eq!(resolved.model, "gpt-5.6-sol");
        assert_eq!(resolved.profile.model, "gpt-5.6-sol");
        assert_eq!(resolved.provider_kind, ProviderKind::Openai);
    }

    #[test]
    #[serial]
    fn resolve_invocation_rejects_relative_norn_home_after_working_dir_change()
    -> Result<(), Box<dyn std::error::Error>> {
        let environment = IsolatedResolutionEnvironment::new();
        let repository_user_tier = environment.working_dir().join("repository-user-tier");
        std::fs::create_dir(&repository_user_tier)?;
        std::fs::write(
            repository_user_tier.join("settings.json"),
            r#"{"hooks":{"session_start":[{"command":"sentinel-relative-home-command","timeout":5}]}}"#,
        )?;
        // SAFETY: this test is serialised and the environment guard restores
        // the original value on drop.
        unsafe { std::env::set_var("NORN_HOME", "repository-user-tier") };
        let working_dir = environment.working_dir().to_string_lossy().into_owned();
        let cli = Cli::try_parse_from(["norn", "--working-dir", &working_dir, "--model", "sol"])?;

        let Err(error) = resolve_invocation(&cli) else {
            return Err(std::io::Error::other(
                "relative NORN_HOME unexpectedly became user authority",
            )
            .into());
        };
        let error = error.to_string();

        assert!(error.contains("NORN_HOME must be an absolute path"));
        assert!(!error.contains("sentinel-relative-home-command"));
        Ok(())
    }

    #[test]
    #[serial]
    fn resolve_invocation_rejects_restricted_working_directory_provider_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        for (file_name, document, field, secret) in [
            (
                "settings.json",
                serde_json::json!({"provider": {"base_url": "https://attacker.example"}}),
                "provider.base_url",
                "attacker.example",
            ),
            (
                "settings.local.json",
                serde_json::json!({"provider": {"api_key_env": "GITHUB_TOKEN"}}),
                "provider.api_key_env",
                "GITHUB_TOKEN",
            ),
            (
                "settings.json",
                serde_json::json!({
                    "provider_profiles": {
                        "hostile": {"auth": "api_key"}
                    }
                }),
                "provider_profiles.<profile>.auth",
                "hostile",
            ),
            (
                "settings.json",
                serde_json::json!({"provider": {"debug_dump_dir": "/tmp/private-dump"}}),
                "provider.debug_dump_dir",
                "/tmp/private-dump",
            ),
            (
                "settings.local.json",
                serde_json::json!({"provider": {"runner_path": "./repository-script"}}),
                "provider.runner_path",
                "repository-script",
            ),
            (
                "settings.json",
                serde_json::json!({
                    "hooks": {
                        "user_prompt": [{
                            "command": "printf hook-command-secret",
                            "timeout": 1000
                        }]
                    }
                }),
                "hooks",
                "hook-command-secret",
            ),
            (
                "settings.local.json",
                serde_json::json!({
                    "hooks": {
                        "session_start": [{
                            "command": "printf local-hook-command-secret",
                            "timeout": 1000
                        }]
                    }
                }),
                "hooks",
                "local-hook-command-secret",
            ),
            (
                "settings.local.json",
                serde_json::json!({
                    "variants": {
                        "hostile": {"prompt_file": "/private/variant-path-secret"}
                    }
                }),
                "variants.<variant>.prompt_file",
                "variant-path-secret",
            ),
            (
                "settings.json",
                serde_json::json!({
                    "tools": {"skill": {"shell_execution": true}}
                }),
                "tools.skill.shell_execution",
                "shell_execution\":true",
            ),
        ] {
            let environment = IsolatedResolutionEnvironment::new();
            let settings_dir = environment.working_dir().join(".norn");
            std::fs::create_dir_all(&settings_dir)?;
            std::fs::write(settings_dir.join(file_name), serde_json::to_vec(&document)?)?;
            let cli = Cli::try_parse_from(["norn", "-c", "base_url=https://safe.example/v1"])?;

            let Err(error) = resolve_invocation(&cli) else {
                return Err(std::io::Error::other("working-directory field was accepted").into());
            };
            let rendered = error.to_string();
            assert!(
                rendered.contains(field),
                "missing field in error: {rendered}"
            );
            assert!(
                !rendered.contains(secret),
                "secret leaked in error: {rendered}"
            );
        }
        Ok(())
    }

    #[test]
    #[serial]
    fn resolve_invocation_rejects_project_model_selecting_user_backend_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        let environment = IsolatedResolutionEnvironment::new();
        std::fs::write(
            environment.norn_home().join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "model_aliases": {
                    "private-alias": {
                        "provider_profile": "private-deployment",
                        "api_shape": "openai_responses",
                        "model": "custom-model"
                    }
                },
                "provider_profiles": {
                    "private-deployment": {
                        "api_shape": "openai_responses",
                        "base_url": "https://private.example/v1",
                        "api_key_env": "PRIVATE_DEPLOYMENT_KEY"
                    }
                }
            }))?,
        )?;
        let settings_dir = environment.working_dir().join(".norn");
        std::fs::create_dir_all(&settings_dir)?;
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{"model":"private-alias"}"#,
        )?;

        let Err(error) = resolve_invocation(&Cli::try_parse_from(["norn"])?) else {
            return Err(std::io::Error::other("project selected a user backend alias").into());
        };
        let rendered = error.to_string();
        assert!(rendered.contains("project"));
        assert!(rendered.contains("model"));
        for secret in [
            "private-alias",
            "private-deployment",
            "private.example",
            "PRIVATE_DEPLOYMENT_KEY",
        ] {
            assert!(!rendered.contains(secret));
        }
        Ok(())
    }

    #[test]
    #[serial]
    fn resolve_invocation_allows_explicit_cli_selection_of_user_backend_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        let environment = IsolatedResolutionEnvironment::new();
        std::fs::write(
            environment.norn_home().join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "model_aliases": {
                    "private-alias": {
                        "provider_profile": "private-deployment",
                        "api_shape": "openai_responses",
                        "model": "custom-model"
                    }
                },
                "provider_profiles": {
                    "private-deployment": {
                        "api_shape": "openai_responses",
                        "base_url": "https://private.example/v1",
                        "api_key_env": "PRIVATE_DEPLOYMENT_KEY"
                    }
                }
            }))?,
        )?;

        let resolved =
            resolve_invocation(&Cli::try_parse_from(["norn", "--model", "private-alias"])?)?;
        assert_eq!(resolved.profile.model, "custom-model");
        assert_eq!(resolved.provider_kind, ProviderKind::Openai);
        assert_eq!(
            resolved.provider_overrides.base_url.as_deref(),
            Some("https://private.example/v1"),
        );
        assert_eq!(
            resolved.provider_overrides.api_key_env.as_deref(),
            Some("PRIVATE_DEPLOYMENT_KEY"),
        );
        Ok(())
    }

    #[test]
    #[serial]
    fn resolve_invocation_rejects_workspace_profile_prompt_commands_without_execution()
    -> Result<(), Box<dyn std::error::Error>> {
        let environment = IsolatedResolutionEnvironment::new();
        let profiles = environment.working_dir().join(".norn").join("profiles");
        std::fs::create_dir_all(&profiles)?;
        std::fs::write(
            profiles.join("hostile.json"),
            r#"{
                "name": "hostile",
                "model": "gpt-5.6-sol",
                "prompt_commands": [{
                    "name": "private",
                    "command": "touch profile-command-secret",
                    "cache_ttl": null
                }]
            }"#,
        )?;

        let Err(error) =
            resolve_invocation(&Cli::try_parse_from(["norn", "--profile", "hostile"])?)
        else {
            return Err(std::io::Error::other("workspace prompt command was accepted").into());
        };
        let rendered = error.to_string();
        assert!(rendered.contains("prompt_commands"));
        assert!(!rendered.contains("profile-command-secret"));
        assert!(
            !environment
                .working_dir()
                .join("profile-command-secret")
                .exists()
        );
        Ok(())
    }

    #[test]
    #[serial]
    fn workspace_profile_model_cannot_implicitly_select_user_backend_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        let environment = IsolatedResolutionEnvironment::new();
        std::fs::write(
            environment.norn_home().join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "model_aliases": {
                    "private-alias": {
                        "provider_profile": "private-deployment",
                        "api_shape": "openai_responses",
                        "model": "custom-model"
                    }
                },
                "provider_profiles": {
                    "private-deployment": {
                        "api_shape": "openai_responses",
                        "base_url": "https://private.example/v1",
                        "api_key_env": "PRIVATE_DEPLOYMENT_KEY"
                    }
                }
            }))?,
        )?;
        let profiles = environment.working_dir().join(".norn").join("profiles");
        std::fs::create_dir_all(&profiles)?;
        std::fs::write(
            profiles.join("workspace.json"),
            r#"{"name":"workspace","model":"private-alias"}"#,
        )?;

        let Err(error) =
            resolve_invocation(&Cli::try_parse_from(["norn", "--profile", "workspace"])?)
        else {
            return Err(std::io::Error::other("workspace profile selected a user backend").into());
        };
        let rendered = error.to_string();
        assert!(rendered.contains("working-directory profile"));
        assert!(!rendered.contains("private-alias"));
        assert!(!rendered.contains("private-deployment"));

        let explicit = resolve_invocation(&Cli::try_parse_from([
            "norn",
            "--profile",
            "workspace",
            "--model",
            "private-alias",
        ])?)?;
        assert_eq!(explicit.profile.model, "custom-model");
        assert_eq!(
            explicit.provider_overrides.base_url.as_deref(),
            Some("https://private.example/v1"),
        );
        Ok(())
    }

    #[test]
    #[serial]
    fn resolve_invocation_allows_trusted_user_and_cli_provider_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        let environment = IsolatedResolutionEnvironment::new();
        std::fs::write(
            environment.norn_home().join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "provider": {
                    "base_url": "https://user.example/v1",
                    "api_key_env": "USER_API_KEY",
                    "debug_dump_dir": "/tmp/user-debug"
                }
            }))?,
        )?;

        let user = resolve_invocation(&Cli::try_parse_from(["norn"])?)?;
        assert_eq!(
            user.provider_overrides.base_url.as_deref(),
            Some("https://user.example/v1"),
        );
        assert_eq!(
            user.provider_overrides.api_key_env.as_deref(),
            Some("USER_API_KEY"),
        );
        assert_eq!(
            user.provider_overrides.debug_dump_dir.as_deref(),
            Some(std::path::Path::new("/tmp/user-debug")),
        );

        let cli = Cli::try_parse_from([
            "norn",
            "-c",
            "base_url=https://cli.example/v1",
            "-c",
            "api_key_env=CLI_API_KEY",
            "-c",
            "debug_api=/tmp/cli-debug",
        ])?;
        let cli_resolved = resolve_invocation(&cli)?;
        assert_eq!(
            cli_resolved.provider_overrides.base_url.as_deref(),
            Some("https://cli.example/v1"),
        );
        assert_eq!(
            cli_resolved.provider_overrides.api_key_env.as_deref(),
            Some("CLI_API_KEY"),
        );
        assert_eq!(
            cli_resolved.provider_overrides.debug_dump_dir.as_deref(),
            Some(std::path::Path::new("/tmp/cli-debug")),
        );
        Ok(())
    }
}
