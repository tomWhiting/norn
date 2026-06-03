//! Stop hook that runs convention diagnostics before an agent exits.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use diagnostics::conventions::TestTrigger;

use crate::integration::hooks::{HookOutcome, StopHook};
use crate::tool::lifecycle::PostValidateOutcome;

use super::infra::DiagnosticInfra;
use super::post_check::run_diagnostics_for_trigger;

/// Stop lifecycle hook for convention-driven diagnostics.
pub struct DiagnosticStopHook {
    infra: Arc<DiagnosticInfra>,
}

impl DiagnosticStopHook {
    /// Create a stop hook sharing the session diagnostic infrastructure.
    #[must_use]
    pub fn new(infra: Arc<DiagnosticInfra>) -> Self {
        Self { infra }
    }
}

#[async_trait]
impl StopHook for DiagnosticStopHook {
    async fn on_stop(&self, _final_text: &str) -> HookOutcome {
        let files: Vec<PathBuf> = self.infra.modified_files().into_iter().collect();
        if files.is_empty() {
            return HookOutcome::Proceed;
        }

        let Some(conventions) = self.infra.conventions.as_ref() else {
            return HookOutcome::Proceed;
        };

        let result = run_diagnostics_for_trigger(
            TestTrigger::Stop,
            None,
            &files,
            conventions,
            self.infra.as_ref(),
        )
        .await;

        match result.outcome {
            PostValidateOutcome::Pass => HookOutcome::Proceed,
            PostValidateOutcome::Fail { errors } if errors.is_empty() => HookOutcome::Proceed,
            PostValidateOutcome::Fail { errors } => HookOutcome::Block {
                reason: format_stop_reason(&errors),
            },
        }
    }
}

fn format_stop_reason(errors: &[String]) -> String {
    let mut reason = String::from("Stop blocked by diagnostic findings:\n");
    for error in errors {
        reason.push_str("- ");
        reason.push_str(error);
        reason.push('\n');
    }
    reason
}
