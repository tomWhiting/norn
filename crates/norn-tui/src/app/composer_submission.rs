//! Draft retirement follows actual local admission or the runner's opening-input receipt.

use norn::provider::agent_event::{ExecutionObservation, PublicationResolution};
use norn::session_view::ItemId;

use super::state::AppState;
use super::transcript::publication::SubmittedInput;
use crate::TuiError;
use crate::input::{ComposerSnapshot, DetachedComposerDraft, InputEditor};

const WAITING: &str = "Waiting for the previous input's acceptance; draft retained";

/// One operator draft awaiting the existing runner's publication decision.
/// This is not a second inbox and is never retried by the frontend.
pub(super) struct PendingSubmission {
    local: ItemId,
    snapshot: ComposerSnapshot,
    draft: DetachedComposerDraft,
    next: ComposerSnapshot,
    observation: Option<ExecutionObservation>,
}

/// Prepare an exact, nonblank draft without clearing or recording it.
pub(super) fn prepare(state: &mut AppState) -> Result<Option<ComposerSnapshot>, TuiError> {
    if state.pending_composer_submission.is_some() {
        state.screen.feedback = Some(WAITING.to_owned());
        state.screen.dirty = true;
        return Ok(None);
    }
    let snapshot = state.input_editor.snapshot()?;
    Ok((!snapshot.text().trim().is_empty()).then_some(snapshot))
}

/// Retain the existing local attempt identity, then let the real runner admit it.
pub(super) fn begin(
    state: &mut AppState,
    snapshot: ComposerSnapshot,
) -> Result<SubmittedInput, TuiError> {
    state.input_editor.validate_snapshot(&snapshot)?;
    if state.pending_composer_submission.is_some() {
        return Err(super::render::interaction(std::io::Error::other(
            "an opening input is already awaiting acceptance",
        )));
    }
    let mut draft = state.input_editor.detach_draft(&snapshot)?;
    let next = match state.input_editor.snapshot() {
        Ok(next) => next,
        Err(error) => {
            state.input_editor.exchange_draft(&mut draft);
            return Err(error.into());
        }
    };
    let input = match super::render::write_user_message(snapshot.text().to_owned(), state) {
        Ok(input) => input,
        Err(error) => {
            state.input_editor.exchange_draft(&mut draft);
            return Err(error);
        }
    };
    state.autocomplete = None;
    state.screen.dirty = true;
    state.pending_composer_submission = Some(PendingSubmission {
        local: input.local.clone(),
        snapshot,
        draft,
        next,
        observation: None,
    });
    Ok(input)
}

/// Bind only the execution that already owns this exact `SubmittedInput` identity.
pub(super) fn bind(
    state: &mut AppState,
    local: Option<&ItemId>,
    observation: Option<&ExecutionObservation>,
) -> Result<(), TuiError> {
    let Some(pending) = state.pending_composer_submission.as_mut() else {
        return Ok(());
    };
    if local != Some(&pending.local) {
        return Ok(());
    }
    let owner = observation.ok_or_else(|| {
        super::render::interaction(std::io::Error::other(
            "composer opening input has no execution observation",
        ))
    })?;
    pending.observation = Some(owner.clone());
    Ok(())
}

/// Resolve on publication, before a delivered input event, or on final drain.
pub(super) fn resolve(state: &mut AppState) -> Result<(), TuiError> {
    let Some(pending) = state.pending_composer_submission.as_ref() else {
        return Ok(());
    };
    let Some(resolution) = pending
        .observation
        .as_ref()
        .and_then(ExecutionObservation::opening_input)
    else {
        return Ok(());
    };
    let accepted = matches!(
        resolution,
        PublicationResolution::Accepted(_) | PublicationResolution::AcceptedButUnavailable { .. }
    );
    let mut pending = state.pending_composer_submission.take().ok_or_else(|| {
        super::render::interaction(std::io::Error::other(
            "resolved composer input lost its pending identity",
        ))
    })?;
    if state.screen.feedback.as_deref() == Some(WAITING) {
        state.screen.feedback = None;
    }
    let untouched = state.input_editor.validate_snapshot(&pending.next).is_ok();
    if untouched {
        state.input_editor.exchange_draft(&mut pending.draft);
    }
    if accepted && untouched {
        accepted_local(state, &pending.snapshot)?;
    } else if accepted {
        let mut issues = Vec::new();
        if !is_secret(pending.snapshot.text())
            && let Err(error) = state
                .input_editor
                .record_detached_accepted(&pending.draft, &pending.snapshot)
        {
            issues.push(format!("Input accepted; recall history could not be saved: {error}. Do not resend to repair history."));
        }
        report_accepted_issues(state, &issues)?;
    } else if untouched {
        state.screen.feedback =
            Some("Input was not accepted; draft and undo history retained".to_owned());
    } else {
        state.composer_recovery.retain_rejected(pending.draft);
        state.screen.feedback = Some(
            "Input was not accepted; new draft kept. Recover rejected message below.".to_owned(),
        );
    }
    state.screen.dirty = true;
    Ok(())
}

/// A successful local dispatch or steer/queue admission retires the draft once.
/// Subsequent recall-write failure is explicitly an accepted-input outcome.
pub(super) fn accepted_local(
    state: &mut AppState,
    snapshot: &ComposerSnapshot,
) -> Result<(), TuiError> {
    let issues = retire_accepted(&mut state.input_editor, snapshot);
    report_accepted_issues(state, &issues)
}

fn report_accepted_issues(state: &mut AppState, issues: &[String]) -> Result<(), TuiError> {
    state.screen.dirty = true;
    if !issues.is_empty() {
        let message = issues.join("\n");
        state.screen.feedback = Some(message.clone());
        super::notices::error(state, "Input accepted", &message).map_err(|source| {
            super::render::interaction(AcceptedNoticeFailure { message, source })
        })?;
    }
    Ok(())
}

/// Retire the already accepted draft, preserving both original and cleanup errors.
pub(super) fn accepted_with_error(
    state: &mut AppState,
    snapshot: &ComposerSnapshot,
    original: TuiError,
) -> TuiError {
    match accepted_local(state, snapshot) {
        Ok(()) => original,
        Err(retirement) => super::render::interaction(AcceptedCleanupFailure {
            original,
            retirement,
        }),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}; reporting this accepted-input outcome failed: {source}")]
struct AcceptedNoticeFailure {
    message: String,
    #[source]
    source: TuiError,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "accepted command failed after its effect: {original}; draft retirement also failed: {retirement}"
)]
struct AcceptedCleanupFailure {
    #[source]
    original: TuiError,
    retirement: TuiError,
}

fn retire_accepted(editor: &mut InputEditor, snapshot: &ComposerSnapshot) -> Vec<String> {
    let mut issues = Vec::new();
    if let Err(error) = editor.clear_accepted(snapshot) {
        issues.push(format!(
            "Input accepted; current draft retained: {error}. It has not been resent."
        ));
    }
    if !is_secret(snapshot.text())
        && let Err(error) = editor.record_accepted(snapshot)
    {
        issues.push(format!("Input accepted; recall history could not be saved: {error}. Do not resend to repair history."));
    }
    issues
}

fn is_secret(text: &str) -> bool {
    norn::integration::is_live_mcp_definition_input(text)
        || text
            .split_whitespace()
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("/auth"))
}

#[cfg(test)]
#[path = "composer_submission_tests.rs"]
mod tests;
