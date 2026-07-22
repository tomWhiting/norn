use std::path::PathBuf;

use norn::config::{NornSettings, ProviderProfileSettings, ProviderSettings};

use super::loop_config::parse_settings_duration;
use crate::cli::BuildError;
use crate::config::{ConfigOverrides, ProviderConfigOverrides};

/// Build provider overrides from merged settings.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when a provider duration is invalid.
pub fn provider_overrides_from_settings(
    settings: &NornSettings,
) -> Result<ProviderConfigOverrides, BuildError> {
    let mut overrides = ProviderConfigOverrides::default();
    let Some(provider) = settings.provider.as_ref() else {
        return Ok(overrides);
    };
    overlay_provider_settings(&mut overrides, "provider", provider)?;
    Ok(overrides)
}

/// Overlay a selected provider profile onto merged provider overrides.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when a profile duration is invalid.
pub fn overlay_provider_profile_overrides(
    overrides: &mut ProviderConfigOverrides,
    profile_name: &str,
    profile: &ProviderProfileSettings,
) -> Result<(), BuildError> {
    overlay_provider_settings(
        overrides,
        &format!("provider_profiles.{profile_name}"),
        &profile.provider,
    )
}

fn overlay_provider_settings(
    overrides: &mut ProviderConfigOverrides,
    prefix: &str,
    provider: &ProviderSettings,
) -> Result<(), BuildError> {
    if let Some(base_url) = provider.base_url.as_deref() {
        overrides.base_url = Some(base_url.to_owned());
    }
    if let Some(timeout) = provider.timeout.as_deref() {
        overrides.request_timeout = Some(parse_settings_duration(
            &format!("{prefix}.timeout"),
            timeout,
        )?);
    }
    if let Some(max_retries) = provider.max_retries {
        overrides.max_retries = Some(max_retries);
    }
    if let Some(options) = provider.options.as_ref() {
        overrides.provider_options = Some(options.clone());
    }
    overlay_provider_auth_fields(overrides, provider.auth, provider.api_key_env.as_deref());
    if let Some(dump_dir) = provider.debug_dump_dir.as_deref() {
        overrides.debug_dump_dir = Some(PathBuf::from(dump_dir));
    }
    if let Some(rate_limit) = provider.rate_limit {
        overrides.rate_limit = Some(rate_limit);
    }
    if let Some(interval) = provider.rate_limit_interval.as_deref() {
        overrides.rate_limit_interval = Some(parse_settings_duration(
            &format!("{prefix}.rate_limit_interval"),
            interval,
        )?);
    }
    if let Some(backoff) = provider.retry_backoff.as_deref() {
        overrides.retry_backoff = Some(parse_settings_duration(
            &format!("{prefix}.retry_backoff"),
            backoff,
        )?);
    }
    if let Some(ceiling) = provider.retry_after_ceiling.as_deref() {
        overrides.retry_after_ceiling = Some(parse_settings_duration(
            &format!("{prefix}.retry_after_ceiling"),
            ceiling,
        )?);
    }
    if let Some(runner_path) = provider.runner_path.as_deref() {
        overrides.runner_path = Some(PathBuf::from(runner_path));
    }
    Ok(())
}

/// Overlay explicit provider values on settings-derived overrides.
pub fn overlay_cli_provider_overrides(
    overrides: &mut ProviderConfigOverrides,
    cli: &ConfigOverrides,
) {
    if let Some(base_url) = cli.base_url.as_deref() {
        overrides.base_url = Some(base_url.to_owned());
    }
    if let Some(max_retries) = cli.max_retries {
        overrides.max_retries = Some(max_retries);
    }
    if let Some(timeout) = cli.request_timeout {
        overrides.request_timeout = Some(timeout);
    }
    if let Some(options) = cli.provider_options.as_ref() {
        overrides.provider_options = Some(options.clone());
    }
    overlay_provider_auth_fields(overrides, cli.auth, cli.api_key_env.as_deref());
    if let Some(dump_dir) = cli.debug_dump_dir.as_ref() {
        overrides.debug_dump_dir = Some(dump_dir.clone());
    }
    if let Some(interval) = cli.rate_limit_interval {
        overrides.rate_limit_interval = Some(interval);
    }
    if let Some(backoff) = cli.retry_backoff {
        overrides.retry_backoff = Some(backoff);
    }
    if let Some(ceiling) = cli.retry_after_ceiling {
        overrides.retry_after_ceiling = Some(ceiling);
    }
}

fn overlay_provider_auth_fields(
    overrides: &mut ProviderConfigOverrides,
    auth: Option<norn::config::ProviderAuthMode>,
    api_key_env: Option<&str>,
) {
    if let Some(name) = api_key_env {
        overrides.api_key_env = Some(name.to_owned());
    } else if auth == Some(norn::config::ProviderAuthMode::OAuth) {
        overrides.api_key_env = None;
    }
    if let Some(mode) = auth {
        overrides.auth = Some(mode);
    }
}
