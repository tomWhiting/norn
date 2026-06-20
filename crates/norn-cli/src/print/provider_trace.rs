//! Startup tracing helpers for provider construction.

use std::path::Path;
use std::time::Instant;

use norn::provider::auth::AuthSource;

use crate::cli::ProviderKind;

const TARGET: &str = "norn_cli::tui::startup";

pub(super) fn provider_build_start(kind: ProviderKind, model: &str) -> Instant {
    tracing::info!(
        target: TARGET,
        stage = "provider_build_start",
        provider = ?kind,
        model,
        "provider build milestone",
    );
    Instant::now()
}

pub(super) fn openai_auth_source_resolved(started_at: Instant, auth_source: &AuthSource) {
    tracing::info!(
        target: TARGET,
        stage = "openai_auth_source_resolved",
        auth_source = auth_source_label(auth_source),
        elapsed_ms = started_at.elapsed().as_millis(),
        "provider build milestone",
    );
}

pub(super) fn openai_provider_new_start(started_at: Instant, base_url_configured: bool) {
    tracing::info!(
        target: TARGET,
        stage = "openai_provider_new_start",
        base_url_configured,
        elapsed_ms = started_at.elapsed().as_millis(),
        "provider build milestone",
    );
}

pub(super) fn openai_provider_new_done(started_at: Instant) {
    tracing::info!(
        target: TARGET,
        stage = "openai_provider_new_done",
        elapsed_ms = started_at.elapsed().as_millis(),
        "provider build milestone",
    );
}

pub(super) fn openai_compatible_api_key_env_resolved(started_at: Instant, api_key_env: &str) {
    tracing::info!(
        target: TARGET,
        stage = "openai_compatible_api_key_env_resolved",
        api_key_env,
        elapsed_ms = started_at.elapsed().as_millis(),
        "provider build milestone",
    );
}

pub(super) fn openai_compatible_provider_new_start(started_at: Instant, base_url_configured: bool) {
    tracing::info!(
        target: TARGET,
        stage = "openai_compatible_provider_new_start",
        base_url_configured,
        elapsed_ms = started_at.elapsed().as_millis(),
        "provider build milestone",
    );
}

pub(super) fn openai_compatible_provider_new_done(started_at: Instant) {
    tracing::info!(
        target: TARGET,
        stage = "openai_compatible_provider_new_done",
        elapsed_ms = started_at.elapsed().as_millis(),
        "provider build milestone",
    );
}

pub(super) fn claude_runner_provider_ready(started_at: Instant, runner_path: &Path) {
    tracing::info!(
        target: TARGET,
        stage = "claude_runner_provider_ready",
        runner_path = %runner_path.display(),
        elapsed_ms = started_at.elapsed().as_millis(),
        "provider build milestone",
    );
}

fn auth_source_label(auth_source: &AuthSource) -> &'static str {
    match auth_source {
        AuthSource::OAuth { .. } => "oauth",
        AuthSource::ApiKey { .. } => "api_key",
    }
}
