//! Provider construction tracepoints used by TUI startup diagnostics.

use std::time::Instant;

const TARGET: &str = "norn_cli::tui::startup";

pub(crate) fn start(stage: &'static str) -> Instant {
    tracing::info!(
        target: TARGET,
        stage,
        "provider construction milestone",
    );
    Instant::now()
}

pub(crate) fn elapsed(stage: &'static str, started_at: Instant) {
    tracing::info!(
        target: TARGET,
        stage,
        elapsed_ms = started_at.elapsed().as_millis(),
        "provider construction milestone",
    );
}

pub(crate) fn auth_manager_load_done(started_at: Instant, credentials_loaded: bool) {
    tracing::info!(
        target: TARGET,
        stage = "oauth_auth_manager_load_auth_done",
        elapsed_ms = started_at.elapsed().as_millis(),
        credentials_loaded,
        "provider construction milestone",
    );
}
