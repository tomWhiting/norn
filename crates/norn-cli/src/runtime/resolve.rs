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

use norn::agent_loop::{
    effort_label, reasoning_effort_supported_for_model, unsupported_reasoning_effort_message,
};
use norn::config::{NornSettings, load_settings, merge_settings, validate_settings};
use norn::profile::Profile;

use crate::cli::{BuildError, Cli, ProviderKind};
use crate::config::{
    AppliedOverrides, ConfigOverrides, ProviderConfigOverrides, apply_cli_profile_overrides,
    apply_settings_reasoning_to_profile, apply_working_dir, overlay_cli_provider_overrides,
    overlay_provider_profile_overrides, provider_overrides_from_settings, resolve_model_selection,
    resolve_profile, resolve_provider_selection,
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
    let mut layers = load_settings(&cwd)?;
    let mut cli_layer = NornSettings::default();
    let settings = merge_settings(
        &mut layers.user,
        &mut layers.project,
        &mut layers.local,
        &mut cli_layer,
    );
    validate_settings(&settings)?;

    let mut profile = resolve_profile(cli.profile.as_deref())?;
    if cli.profile.is_none()
        && cli.model.is_none()
        && let Some(model) = settings.model.as_deref()
    {
        model.clone_into(&mut profile.model);
    }
    apply_settings_reasoning_to_profile(&settings, &mut profile)?;
    let applied = apply_cli_profile_overrides(cli, &mut profile)?;
    let model_selection = resolve_model_selection(&profile.model, &settings)?;
    profile.model.clone_from(&model_selection.model);
    validate_reasoning_effort_for_model(&profile)?;

    let mut config_overrides = ConfigOverrides::parse(&cli.config)?;
    if let Some(debug_api) = &cli.debug_api {
        config_overrides.debug_dump_dir = Some(resolve_debug_api_dir(debug_api));
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

    let model = profile.model.clone();
    Ok(ResolvedInvocation {
        settings,
        profile,
        applied,
        provider_kind: provider_selection.kind,
        provider_overrides,
        model,
        delegation_depth,
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
/// `~/.norn/debug` (falling back to `.norn/debug` when the home dir
/// cannot be resolved).
fn resolve_debug_api_dir(value: &str) -> std::path::PathBuf {
    use std::path::PathBuf;
    if value.is_empty() {
        return crate::config::paths::norn_dir()
            .unwrap_or_else(|| PathBuf::from(".norn"))
            .join("debug");
    }
    PathBuf::from(value)
}
