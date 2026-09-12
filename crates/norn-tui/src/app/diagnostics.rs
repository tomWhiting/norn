//! Retain process diagnostics on the UI owner without granting runtime or input authority.

use super::state::AppState;
use crate::TuiError;
use crate::diagnostics::{Diagnostic, DiagnosticReceiver};
use tokio::sync::broadcast::error::RecvError;

pub(super) async fn wait(
    receiver: &mut Option<DiagnosticReceiver>,
) -> Result<Diagnostic, RecvError> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

pub(super) fn finish(
    state: &mut AppState,
    result: Result<Diagnostic, RecvError>,
) -> Result<(), TuiError> {
    match result {
        Ok(event) => {
            let label = format!("{} · {}", event.level, event.target);
            let item = if event.level == tracing::Level::ERROR {
                super::notices::error(state, &label, &event.text)?
            } else {
                super::notices::notice(state, &label, Some(&event.text))?
            };
            state.screen.diagnostic_items.insert(item);
            if let Some(receiver) = &state.diagnostics {
                receiver.acknowledge(event.sequence);
            }
        }
        Err(RecvError::Lagged(missed)) => {
            super::notices::notice(
                state,
                &format!("Diagnostics: {missed} pending events overwritten before display"),
                None,
            )?;
        }
        Err(RecvError::Closed) => {
            state.diagnostics = None;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
