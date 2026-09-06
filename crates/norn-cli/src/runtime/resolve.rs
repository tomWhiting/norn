//! Shared CLI resolution — the profile, settings, and provider selection
//! every driver (print, driven, TUI) resolves before handing off to
//! [`builder_from_cli`](crate::runtime::builder_from_cli).
//!
//! Provider construction is a CLI config surface, not library assembly:
//! the concrete [`Provider`](norn::provider::traits::Provider) is built
//! from the resolved model and overrides *before* the builder runs, then
//! passed into `builder_from_cli`. This module owns the resolution that
//! precedes that construction — model-alias and provider-profile
//! resolution, settings merge, CLI profile overrides, and complete model-selection
//! validation — so the three drivers share one code path instead of
//! re-deriving it.

use std::time::Duration;

use norn::config::{McpConfigState, McpRuntimeOverrides, NornSettings, ResolvedMcpServers};
use norn::model_selection::{CatalogBackend, ModelInput, ModelRuntime};
use norn::profile::Profile;
use norn::runtime_init::load_resolved_settings;

use crate::cli::{BuildError, Cli, ProviderKind};
use crate::config::{
    AppliedOverrides, CliProfileSource, ConfigOverrides, ProviderConfigOverrides,
    apply_cli_profile_overrides, apply_settings_reasoning_to_profile, apply_working_dir,
    collect_mcp_launch_servers, overlay_cli_provider_overrides, overlay_provider_profile_overrides,
    provider_overrides_from_settings, resolve_index_lock_deadline, resolve_model_selection,
    resolve_profile_with_origin, resolve_provider_auth, resolve_provider_selection,
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
    /// Frontend objects from the existing settings load, preserving whole-layer provenance.
    pub tui_preferences: norn::config::TuiPreferencesLayers,
    /// The merged, validated settings both the provider construction and
    /// the builder's `load_runtime_base` consult.
    pub settings: NornSettings,
    /// Canonical project root used by MCP roots and shared-project approval.
    pub project_root: std::path::PathBuf,
    /// Effective MCP definitions with winning-layer provenance.
    pub mcp_servers: ResolvedMcpServers,
    /// Reloadable MCP configuration with every raw source layer retained.
    pub mcp_state: McpConfigState,
    /// Explicit named channel admission and quotas, absent unless requested.
    pub channel_config: Option<norn::integration::McpChannelSettings>,
    /// The resolved profile with model / tool / reasoning overrides
    /// applied, ready to move into `builder_from_cli`.
    pub profile: Profile,
    /// Mandatory source classification for the profile prompt.
    pub profile_source: CliProfileSource,
    /// The applied-overrides side channel (disallowed tools, unmatched
    /// tool flag names) `builder_from_cli` consumes.
    pub applied: AppliedOverrides,
    /// The selected provider backend.
    pub provider_kind: ProviderKind,
    /// Named provider profile retained for live alias validation.
    pub provider_profile: Option<String>,
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
/// overrides, resolves the model alias and concrete provider route, and
/// preflights the complete model, context window, effort and tier selection.
///
/// # Errors
///
/// [`BuildError`] when the working directory cannot be applied, the
/// settings fail to load / validate, the profile or model cannot be
/// resolved, the selected model's route cannot validate its context window,
/// effort or service tier, or the provider selection / overrides fail to resolve.
pub fn resolve_invocation(cli: &Cli) -> Result<ResolvedInvocation, BuildError> {
    apply_working_dir(cli)?;

    let cwd = std::env::current_dir()?;
    let mcp_overrides = McpRuntimeOverrides {
        cli: collect_mcp_launch_servers(&cli.mcp_config, &cli.extension)?,
        session: std::collections::BTreeMap::new(),
    };
    let mcp_state = McpConfigState::load(&cwd, mcp_overrides.cli.clone())
        .map_err(|error| BuildError::Argument(error.to_string()))?;
    let resolved_settings = load_resolved_settings(&cwd, &mcp_overrides)
        .map_err(|error| BuildError::Argument(error.to_string()))?;
    let mut config_overrides = ConfigOverrides::parse(&cli.config)?;
    let channel_config = crate::runtime::resolve_channel_config(
        cli,
        resolved_settings.settings.channels.as_ref(),
        config_overrides.channels.as_ref(),
        &resolved_settings.mcp_servers,
    )?;
    let settings = resolved_settings.settings;

    let resolved_profile = resolve_profile_with_origin(cli.profile.as_deref())?;
    let profile_is_working_directory_controlled = resolved_profile.working_directory_controlled;
    let profile_source = resolved_profile.profile_source;
    let mut profile = resolved_profile.profile;
    if cli.profile.is_none()
        && cli.model.is_none()
        && let Some(model) = settings.model.as_deref()
    {
        model.clone_into(&mut profile.model);
    }
    let mut effort_source = if cli.reasoning_effort.is_some() {
        "--reasoning-effort"
    } else if profile.reasoning_effort.is_some() {
        "profile.reasoning_effort"
    } else {
        "agent.reasoning_effort"
    };
    let tier_source = if cli.fast {
        "--fast"
    } else if cli.service_tier.is_some() {
        "--service-tier"
    } else if profile.service_tier.is_some() {
        "profile.service_tier"
    } else {
        "agent.service_tier"
    };
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
    let auth = resolve_provider_auth(provider_selection.kind, &provider_overrides)
        .map_err(|error| BuildError::Argument(error.to_string()))?;
    let backend = match provider_selection.kind {
        ProviderKind::Openai => Some(match auth {
            norn::config::ResolvedProviderAuth::OAuth => {
                norn::model_selection::CatalogBackend::CODEX
            }
            norn::config::ResolvedProviderAuth::ApiKeyEnv(_) => {
                norn::model_selection::CatalogBackend::RESPONSES
            }
            norn::config::ResolvedProviderAuth::None => {
                return Err(BuildError::Argument(
                    "OpenAI auth resolution did not select a backend".to_owned(),
                ));
            }
        }),
        ProviderKind::OpenaiCompatible => Some(norn::model_selection::CatalogBackend::CHAT),
        ProviderKind::ClaudeRunner => None,
    };
    if profile.reasoning_effort.is_none() {
        profile.reasoning_effort =
            norn::model_selection::defaults::reasoning_effort(backend, &profile.model);
        effort_source = "Norn default reasoning effort";
    }
    let explicit_window = config_overrides.context_window.or_else(|| {
        settings
            .agent
            .as_ref()
            .and_then(|agent| agent.context_window)
    });
    let mut selection_sources = vec![if cli.model.is_some() {
        "--model"
    } else if cli.profile.is_none() && settings.model.is_some() {
        "settings.model"
    } else {
        "profile.model"
    }];
    if profile.reasoning_effort.is_some() {
        selection_sources.push(effort_source);
    }
    if profile.service_tier.is_some() {
        selection_sources.push(tier_source);
    }
    if explicit_window.is_some() {
        selection_sources.push(if config_overrides.context_window.is_some() {
            "-c context_window"
        } else {
            "agent.context_window"
        });
    }
    preflight_model_selection(&profile, backend, explicit_window, &selection_sources)?;
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
        tui_preferences: resolved_settings.tui_preferences,
        settings,
        project_root: resolved_settings.project_root,
        mcp_servers: resolved_settings.mcp_servers,
        mcp_state,
        channel_config,
        profile,
        profile_source,
        applied,
        provider_kind: provider_selection.kind,
        provider_profile: provider_selection.profile_name,
        provider_overrides,
        model,
        delegation_depth,
        index_lock_deadline,
    })
}

/// Preflight the whole resolved selection before provider construction. The library
/// repeats this boundary against its concrete provider before admitting an agent.
fn preflight_model_selection(
    profile: &Profile,
    backend: Option<CatalogBackend>,
    explicit_window: Option<u64>,
    sources: &[&str],
) -> Result<(), BuildError> {
    ModelRuntime::from_input(
        backend,
        ModelInput::Resolved(profile.model.clone()),
        explicit_window,
        profile.reasoning_effort,
        profile.service_tier,
        std::collections::BTreeMap::new(),
    )
    .map(drop)
    .map_err(|error| {
        BuildError::Argument(format!("model selection ({}): {error}", sources.join(", ")))
    })
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
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use clap::Parser;
    use serial_test::serial;

    use super::*;
    use norn::provider::mock::MockProvider;
    use norn::system_prompt::{PromptAuthority, PromptSource};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct IsolatedResolutionEnvironment {
        norn_home: tempfile::TempDir,
        working_dir: tempfile::TempDir,
    }

    impl IsolatedResolutionEnvironment {
        fn run(test: impl FnOnce(&Self) -> TestResult) -> TestResult {
            let environment = Self {
                norn_home: tempfile::tempdir()?,
                working_dir: tempfile::tempdir()?,
            };
            temp_env::with_var("NORN_HOME", Some(environment.norn_home()), || {
                let directory = ResolutionDirectory::enter(environment.working_dir())?;
                let outcome = test(&environment);
                directory.finish(outcome)
            })
        }

        fn norn_home(&self) -> &std::path::Path {
            self.norn_home.path()
        }

        fn working_dir(&self) -> &std::path::Path {
            self.working_dir.path()
        }
    }

    struct ResolutionDirectory {
        previous: Option<PathBuf>,
    }

    impl ResolutionDirectory {
        fn enter(path: &std::path::Path) -> std::io::Result<Self> {
            let previous = std::env::current_dir()?;
            std::env::set_current_dir(path)?;
            Ok(Self {
                previous: Some(previous),
            })
        }

        fn finish(mut self, outcome: TestResult) -> TestResult {
            let previous = self.previous.take().ok_or_else(|| {
                std::io::Error::other("resolution directory restoration was already consumed")
            })?;
            match std::env::set_current_dir(&previous) {
                Ok(()) => outcome,
                Err(source) => Err(Box::new(DirectoryRestoration {
                    path: previous,
                    source,
                    test_error: outcome.err(),
                })),
            }
        }
    }

    impl Drop for ResolutionDirectory {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous
                && let Err(error) = std::env::set_current_dir(previous)
            {
                tracing::error!(path = %previous.display(), %error, "failed to restore resolution-test working directory during unwind");
            }
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error(
        "restoring resolution-test working directory {path}: {source}; test error: {test_error:?}"
    )]
    struct DirectoryRestoration {
        path: PathBuf,
        source: std::io::Error,
        test_error: Option<Box<dyn std::error::Error>>,
    }

    #[test]
    #[serial]
    fn isolation_restores_working_directory_and_home_after_test_error() -> TestResult {
        temp_env::with_var("NORN_HOME", Some("resolution-outer-value"), || {
            let previous_dir = std::env::current_dir()?;
            let previous_home = std::env::var_os("NORN_HOME");
            let outcome = IsolatedResolutionEnvironment::run(|environment| {
                assert_eq!(
                    std::env::current_dir()?,
                    environment.working_dir().canonicalize()?
                );
                assert_eq!(
                    std::env::var_os("NORN_HOME"),
                    Some(environment.norn_home().as_os_str().to_owned())
                );
                Err(std::io::Error::other("resolution fixture early error").into())
            });
            let error = outcome.err().ok_or_else(|| {
                std::io::Error::other("resolution fixture unexpectedly discarded its early error")
            })?;
            assert_eq!(error.to_string(), "resolution fixture early error");
            assert_eq!(std::env::current_dir()?, previous_dir);
            assert_eq!(std::env::var_os("NORN_HOME"), previous_home);
            Ok(())
        })
    }

    #[test]
    fn selection_preflight_error_has_route_effort_and_no_cli_prefix()
    -> Result<(), Box<dyn std::error::Error>> {
        let profile = Profile {
            model: "gpt-5.6-luna".to_owned(),
            reasoning_effort: Some(norn::provider::request::ReasoningEffort::Ultra),
            ..Profile::default()
        };
        let Err(error) = preflight_model_selection(
            &profile,
            Some(CatalogBackend::CODEX),
            None,
            &["--reasoning-effort"],
        ) else {
            return Err("Luna ultra effort must fail before assembly".into());
        };
        assert!(matches!(error, BuildError::Argument(_)));
        let message = error.to_string();
        assert!(!message.contains("norn:"));
        assert_eq!(message.matches("openai.codex_subscription").count(), 1);
        assert_eq!(message.matches("'ultra'").count(), 1);
        Ok(())
    }

    #[test]
    #[serial]
    fn resolve_invocation_canonicalizes_cli_model_catalog_alias() -> TestResult {
        IsolatedResolutionEnvironment::run(|_| {
            let cli = Cli::try_parse_from(["norn", "--model", "sol"])?;
            let resolved = resolve_invocation(&cli)?;

            assert_eq!(resolved.model, "gpt-5.6-sol");
            assert_eq!(resolved.profile.model, "gpt-5.6-sol");
            assert_eq!(resolved.provider_kind, ProviderKind::Openai);

            Ok(())
        })
    }

    #[test]
    #[serial]
    fn resolve_invocation_rejects_relative_norn_home_after_working_dir_change()
    -> Result<(), Box<dyn std::error::Error>> {
        IsolatedResolutionEnvironment::run(|environment| {
            let repository_user_tier = environment.working_dir().join("repository-user-tier");
            std::fs::create_dir(&repository_user_tier)?;
            std::fs::write(
                repository_user_tier.join("settings.json"),
                r#"{"hooks":{"session_start":[{"command":"sentinel-relative-home-command","timeout":5}]}}"#,
            )?;
            temp_env::with_var("NORN_HOME", Some("repository-user-tier"), || {
                let working_dir = environment.working_dir().to_string_lossy().into_owned();
                let cli =
                    Cli::try_parse_from(["norn", "--working-dir", &working_dir, "--model", "sol"])?;

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
            })
        })
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
            IsolatedResolutionEnvironment::run(|environment| {
                let settings_dir = environment.working_dir().join(".norn");
                std::fs::create_dir_all(&settings_dir)?;
                std::fs::write(settings_dir.join(file_name), serde_json::to_vec(&document)?)?;
                let cli = Cli::try_parse_from(["norn", "-c", "base_url=https://safe.example/v1"])?;

                let Err(error) = resolve_invocation(&cli) else {
                    return Err(
                        std::io::Error::other("working-directory field was accepted").into(),
                    );
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
                Ok(())
            })?;
        }
        Ok(())
    }

    #[test]
    #[serial]
    fn hostile_project_auth_value_is_not_rendered() -> Result<(), Box<dyn std::error::Error>> {
        IsolatedResolutionEnvironment::run(|environment| {
            let settings_dir = environment.working_dir().join(".norn");
            std::fs::create_dir_all(&settings_dir)?;
            std::fs::write(
                settings_dir.join("settings.json"),
                r#"{"provider":{"auth":"AUTH_VALUE_MUST_NOT_APPEAR\u001b[31m"}}"#,
            )?;

            let Err(error) = resolve_invocation(&Cli::try_parse_from(["norn"])?) else {
                return Err(std::io::Error::other("hostile project auth mode was accepted").into());
            };
            let rendered = error.to_string();
            assert!(rendered.contains("expected exactly oauth or api_key"));
            assert!(!rendered.contains("AUTH_VALUE_MUST_NOT_APPEAR"));
            assert!(!rendered.contains('\u{1b}'));
            assert!(!rendered.contains("[31m"));
            Ok(())
        })
    }

    #[test]
    #[serial]
    fn resolve_invocation_rejects_project_model_selecting_user_backend_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        IsolatedResolutionEnvironment::run(|environment| {
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
        })
    }

    #[test]
    #[serial]
    fn resolve_invocation_allows_explicit_cli_selection_of_user_backend_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        IsolatedResolutionEnvironment::run(|environment| {
            std::fs::write(
                environment.norn_home().join("settings.json"),
                serde_json::to_vec(&serde_json::json!({
                    "agent": {"context_window": 272_000},
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
        })
    }

    #[test]
    #[serial]
    fn resolve_invocation_rejects_workspace_profile_prompt_commands_without_execution()
    -> Result<(), Box<dyn std::error::Error>> {
        IsolatedResolutionEnvironment::run(|environment| {
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
        })
    }

    #[test]
    #[serial]
    fn workspace_profile_stays_user_authority_through_cli_assembly()
    -> Result<(), Box<dyn std::error::Error>> {
        IsolatedResolutionEnvironment::run(|environment| {
            let profiles = environment.working_dir().join(".norn").join("profiles");
            std::fs::create_dir_all(&profiles)?;
            std::fs::write(
                profiles.join("workspace.json"),
                serde_json::json!({
                    "name": "workspace",
                    "model": "gpt-5.6-sol",
                    "system_instructions": ["WORKSPACE_PROFILE_AUTHORITY_SENTINEL"]
                })
                .to_string(),
            )?;
            let cli = Cli::try_parse_from(["norn", "--profile", "workspace", "--no-session"])?;
            let resolved = resolve_invocation(&cli)?;
            assert_eq!(
                resolved.profile_source,
                CliProfileSource::Discovered(norn::profile::ProfileOrigin::WorkingDirectory)
            );

            let parts = crate::runtime::builder_from_cli(
                &cli,
                Arc::new(MockProvider::new(Vec::new())),
                resolved.profile,
                resolved.profile_source,
                &resolved.settings,
                &resolved.applied,
            )?
            .build()?
            .into_parts();
            let plan = parts.loop_context.stable_prompt_plan().ok_or_else(|| {
                std::io::Error::other("CLI assembly omitted the typed prompt plan")
            })?;
            let workspace_fragment = plan
                .fragments()
                .iter()
                .find(|fragment| fragment.source() == PromptSource::WorkspaceProfile)
                .ok_or_else(|| {
                    std::io::Error::other("workspace profile fragment was not preserved")
                })?;
            assert_eq!(workspace_fragment.authority(), PromptAuthority::User);
            assert_eq!(
                workspace_fragment.content(),
                "WORKSPACE_PROFILE_AUTHORITY_SENTINEL"
            );
            assert!(plan.fragments().iter().all(|fragment| {
                fragment.authority() == PromptAuthority::User
                    || !fragment
                        .content()
                        .contains("WORKSPACE_PROFILE_AUTHORITY_SENTINEL")
            }));
            Ok(())
        })
    }

    #[test]
    #[serial]
    fn workspace_profile_model_cannot_implicitly_select_user_backend_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        IsolatedResolutionEnvironment::run(|environment| {
            std::fs::write(
                environment.norn_home().join("settings.json"),
                serde_json::to_vec(&serde_json::json!({
                    "agent": {"context_window": 272_000},
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
                return Err(
                    std::io::Error::other("workspace profile selected a user backend").into(),
                );
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
        })
    }

    #[test]
    #[serial]
    fn resolve_invocation_allows_trusted_user_and_cli_provider_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        IsolatedResolutionEnvironment::run(|environment| {
            std::fs::write(
                environment.norn_home().join("settings.json"),
                serde_json::to_vec(&serde_json::json!({
                    "agent": {"context_window": 272_000},
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
        })
    }
}
