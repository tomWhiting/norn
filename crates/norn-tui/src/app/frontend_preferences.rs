//! One frontend save owner: no timers, provider effects or unobserved background transactions.

use super::render::interaction;
use super::slash::LocalCommandOutcome;
use super::state::AppState;
use crate::TuiError;
use crate::frontend_preferences::{
    FrontendPreferences, FrontendPreferencesLaunch, PreferenceScope,
};
use norn::config::{
    SettingsPublication, TuiPreferenceLayer, TuiPreferencesChange, TuiPreferencesSnapshot,
};
use std::sync::Arc;
use tokio::task::JoinHandle;

pub(super) type SaveResult =
    Result<Result<TuiPreferencesChange, norn::config::TuiPreferencesError>, tokio::task::JoinError>;

struct PendingSave {
    scope: PreferenceScope,
    requested: FrontendPreferences,
    path: std::path::PathBuf,
    task: JoinHandle<Result<TuiPreferencesChange, norn::config::TuiPreferencesError>>,
}

#[derive(Debug, thiserror::Error)]
enum SaveError {
    #[error("frontend preference scope {scope:?} has no captured save authority")]
    Authority { scope: PreferenceScope },
    #[error("frontend preference save task failed: {0}")]
    Task(#[source] tokio::task::JoinError),
    #[error(transparent)]
    Patch(#[from] norn::config::TuiPreferencesError),
    #[error("frontend preference completion has no pending transaction")]
    MissingPending,
    #[error(
        "previous frontend save task ended without an outcome; inspect settings and restart to capture a fresh snapshot before saving again"
    )]
    UnresolvedOutcome,
}

enum SaveStatus {
    Loaded,
    Saved,
    Unchanged,
    Failed(Arc<SaveError>),
    DurabilityUncertain(Arc<norn::config::SettingsDocumentError>),
}

/// Owned save lifecycle, separate from session state and terminal geometry.
pub(super) struct PreferenceOwner {
    current: FrontendPreferences,
    scope: PreferenceScope,
    user: Option<TuiPreferencesSnapshot>,
    local: Option<TuiPreferencesSnapshot>,
    winner: Option<TuiPreferenceLayer>,
    dirty: bool,
    outcome_unknown: bool,
    last_target: Option<(PreferenceScope, std::path::PathBuf)>,
    blocked: bool,
    pending: Option<PendingSave>,
    status: SaveStatus,
}

impl PreferenceOwner {
    pub fn new(launch: FrontendPreferencesLaunch) -> Self {
        Self {
            current: launch.initial,
            scope: launch.scope,
            user: launch.user,
            local: launch.local,
            winner: launch.winner,
            dirty: false,
            outcome_unknown: false,
            last_target: None,
            blocked: false,
            pending: None,
            status: SaveStatus::Loaded,
        }
    }

    fn target(&self) -> Option<&TuiPreferencesSnapshot> {
        match self.scope {
            PreferenceScope::Run => None,
            PreferenceScope::User => self.user.as_ref(),
            PreferenceScope::Local => self.local.as_ref(),
        }
    }

    fn start(&mut self, explicit: bool) -> Result<bool, TuiError> {
        if self.outcome_unknown {
            return if explicit {
                Err(interaction(SaveError::UnresolvedOutcome))
            } else {
                Ok(false)
            };
        }
        if self.pending.is_some()
            || self.scope == PreferenceScope::Run
            || (!explicit && (!self.dirty || self.blocked))
        {
            return Ok(false);
        }
        let snapshot = self
            .target()
            .cloned()
            .ok_or_else(|| interaction(SaveError::Authority { scope: self.scope }))?;
        let projection = self.current.projection()?;
        self.pending = Some(PendingSave {
            scope: self.scope,
            requested: self.current.clone(),
            path: snapshot.path().to_path_buf(),
            task: tokio::task::spawn_blocking(move || {
                snapshot.patch(&["view", "display", "input", "composer"], &projection)
            }),
        });
        Ok(true)
    }

    fn summary(&self) -> String {
        let state = if self.pending.is_some() {
            "pending"
        } else if self.outcome_unknown {
            "outcome unknown"
        } else if matches!(self.status, SaveStatus::Failed(_)) {
            "failed"
        } else if self.dirty {
            "unsaved"
        } else {
            "loaded/saved"
        };
        let outcome = match &self.status {
            SaveStatus::Loaded => "No preference write in this process".to_owned(),
            SaveStatus::Saved => "Published and directory sync confirmed".to_owned(),
            SaveStatus::Unchanged => "Document already contained the requested values".to_owned(),
            SaveStatus::Failed(error) if self.outcome_unknown => {
                format!("Save outcome unknown; publication may have happened: {error}")
            }
            SaveStatus::Failed(error) => format!("Save failed before publication: {error}"),
            SaveStatus::DurabilityUncertain(error) => {
                format!("Settings published; durability uncertain: {error}")
            }
        };
        let shadowed = self.scope == PreferenceScope::User
            && matches!(
                self.winner,
                Some(TuiPreferenceLayer::SharedProject | TuiPreferenceLayer::WorkspaceLocal)
            );
        let pending = self.pending.as_ref().map_or_else(
            || "none".to_owned(),
            |pending| format!("{:?} {}", pending.scope, pending.path.display()),
        );
        let last = self.last_target.as_ref().map_or_else(
            || "none".to_owned(),
            |(scope, path)| format!("{scope:?} {}", path.display()),
        );
        format!(
            "Pending transaction: {pending}\nLast completed transaction target: {last}\nPreference scope: {:?}\nTarget: {}\nSave state: {state}\n{outcome}\nEffective whole-object layer captured at launch/last own publication: {:?}\nPersonal save shadowed by higher layer: {shadowed}\nRun values remain active; higher layers still win on restart.",
            self.scope,
            self.target().map_or_else(
                || "temporary run; no document".to_owned(),
                |target| target.path().display().to_string()
            ),
            self.winner
        )
    }
}

fn capture(state: &AppState) -> FrontendPreferences {
    FrontendPreferences {
        changes_open: state.screen.changes_open,
        split: state.screen.split,
        upper: state.screen.upper,
        view: state.transcript.config.clone(),
        display: state.display_toggles,
        submit_mode: state.in_flight_input.mode(),
        composer_send_key: state.composer_send_key,
    }
}

pub(super) fn install(state: &mut AppState, launch: FrontendPreferencesLaunch) {
    let initial = &launch.initial;
    state.screen.changes_open = initial.changes_open;
    state.screen.split = initial.split;
    state.screen.upper = initial.upper;
    state.transcript.config = initial.view.clone();
    state.display_toggles = initial.display;
    state.in_flight_input.set_mode(initial.submit_mode);
    state.composer_send_key = initial.composer_send_key;
    state.preferences = PreferenceOwner::new(launch);
}

/// Called by preference-editing input paths only; unchanged geometry is not a save trigger.
pub(super) fn edited(state: &mut AppState) -> Result<(), TuiError> {
    let current = capture(state);
    if current != state.preferences.current {
        state.preferences.current = current;
        state.preferences.dirty = true;
        state.preferences.start(false)?;
    }
    Ok(())
}

/// Borrowing the existing task makes select cancellation harmless.
pub(super) async fn wait(owner: &mut PreferenceOwner) -> SaveResult {
    match owner.pending.as_mut() {
        Some(pending) => (&mut pending.task).await,
        None => std::future::pending().await,
    }
}

pub(super) fn finish(state: &mut AppState, result: SaveResult) -> Result<(), TuiError> {
    let pending = state
        .preferences
        .pending
        .take()
        .ok_or_else(|| interaction(SaveError::MissingPending))?;
    state.preferences.last_target = Some((pending.scope, pending.path));
    let result = match result {
        Ok(result) => result.map_err(SaveError::Patch),
        Err(error) => Err(SaveError::Task(error)),
    };
    let mut notice = None;
    let succeeded = match result {
        Ok(change) => {
            match pending.scope {
                PreferenceScope::User => {
                    state.preferences.user = Some(change.snapshot);
                    if state.preferences.winner.is_none() {
                        state.preferences.winner = Some(TuiPreferenceLayer::User);
                    }
                }
                PreferenceScope::Local => {
                    state.preferences.local = Some(change.snapshot);
                    state.preferences.winner = Some(TuiPreferenceLayer::WorkspaceLocal);
                }
                PreferenceScope::Run => return Err(interaction(SaveError::MissingPending)),
            }
            if pending.scope == state.preferences.scope && pending.requested == capture(state) {
                state.preferences.dirty = false;
            }
            state.preferences.blocked = false;
            if pending.scope == PreferenceScope::User
                && matches!(
                    state.preferences.winner,
                    Some(TuiPreferenceLayer::SharedProject | TuiPreferenceLayer::WorkspaceLocal)
                )
            {
                state.screen.feedback = Some("Personal preferences saved but shadowed on restart; /view preferences status or local".to_owned());
                state.screen.dirty = true;
            }
            state.preferences.status = match change.publication {
                SettingsPublication::Unchanged => SaveStatus::Unchanged,
                SettingsPublication::PublishedDurable => SaveStatus::Saved,
                SettingsPublication::PublishedDurabilityUncertain(error) => {
                    notice = Some(("Preference save durability", error.to_string()));
                    SaveStatus::DurabilityUncertain(Arc::new(error))
                }
            };
            true
        }
        Err(error) => {
            state.preferences.outcome_unknown = matches!(error, SaveError::Task(_));
            let label = if state.preferences.outcome_unknown {
                "Preference save outcome unknown"
            } else {
                "Preference save failed"
            };
            notice = Some((label, error.to_string()));
            state.preferences.status = SaveStatus::Failed(Arc::new(error));
            state.preferences.blocked = true;
            state.preferences.dirty = true;
            false
        }
    };
    if let Some((label, detail)) = notice {
        super::notices::error(state, label, &detail)?;
    }
    if succeeded {
        state.preferences.start(false)?;
    }
    Ok(())
}

/// Explicit commands stay local even during steer/queue admission.
pub(super) fn command(
    arguments: &str,
    state: &mut AppState,
) -> Result<LocalCommandOutcome, TuiError> {
    let outcome = match arguments.trim() {
        "" | "status" => LocalCommandOutcome::Accepted,
        "run" | "user" | "local" => {
            state.preferences.scope = match arguments.trim() {
                "run" => PreferenceScope::Run,
                "user" => PreferenceScope::User,
                _ => PreferenceScope::Local,
            };
            state.preferences.dirty = true;
            state.preferences.blocked = false;
            if let Err(error) = state.preferences.start(false) {
                let reporting = command_failure(state, &error);
                return Ok(LocalCommandOutcome::after_reported_failure(
                    error, reporting,
                ));
            }
            LocalCommandOutcome::Accepted
        }
        "save" => {
            if state.preferences.scope == PreferenceScope::Run {
                state.screen.feedback = Some("Temporary run preferences: choose /view preferences user or local before saving".to_owned());
                LocalCommandOutcome::Rejected
            } else if state.preferences.pending.is_some() {
                state.screen.feedback = Some(
                    "Preference save pending; latest edits remain unsaved until its outcome"
                        .to_owned(),
                );
                LocalCommandOutcome::Accepted
            } else {
                if let Err(error) = state.preferences.start(true) {
                    command_failure(state, &error)?;
                    return Ok(LocalCommandOutcome::Rejected);
                }
                LocalCommandOutcome::Accepted
            }
        }
        _ => {
            state.screen.feedback =
                Some("Use /view preferences status|run|user|local|save".to_owned());
            return Ok(LocalCommandOutcome::Rejected);
        }
    };
    let reporting = (|| {
        let detail = format!(
            "{}\n\nActive preferences\n{}",
            state.preferences.summary(),
            ValueDisplay(capture(state).projection()?)
        );
        let item = super::notices::notice(state, "Frontend preferences", Some(&detail))?;
        state
            .screen
            .viewport
            .scroll_to(
                super::viewport::ViewAnchor {
                    item,
                    position: super::viewport::AnchorPosition::Header,
                },
                &state.transcript.projection,
            )
            .map_err(interaction)
    })();
    match outcome {
        LocalCommandOutcome::Accepted => Ok(LocalCommandOutcome::after_acceptance(reporting)),
        LocalCommandOutcome::Rejected => {
            reporting?;
            Ok(LocalCommandOutcome::Rejected)
        }
        LocalCommandOutcome::AcceptedWithError(error) => Ok(
            LocalCommandOutcome::after_reported_failure(error, reporting),
        ),
    }
}

fn command_failure(state: &mut AppState, error: &TuiError) -> Result<(), TuiError> {
    let item = super::notices::error(state, "View command", &error.to_string())?;
    state
        .screen
        .viewport
        .scroll_to(
            super::viewport::ViewAnchor {
                item,
                position: super::viewport::AnchorPosition::Header,
            },
            &state.transcript.projection,
        )
        .map_err(interaction)
}

struct ValueDisplay(serde_json::Map<String, serde_json::Value>);
impl std::fmt::Display for ValueDisplay {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", serde_json::Value::Object(self.0.clone()))
    }
}

/// Observe every accepted transaction on ordinary exit, including any latest pending edit.
pub(super) async fn drain(state: &mut AppState) -> Result<(), TuiError> {
    edited(state)?;
    while state.preferences.pending.is_some() {
        let result = wait(&mut state.preferences).await;
        finish(state, result)?;
    }
    match &state.preferences.status {
        SaveStatus::Failed(error) => Err(interaction(Arc::clone(error))),
        SaveStatus::DurabilityUncertain(error) => Err(interaction(Arc::clone(error))),
        SaveStatus::Loaded | SaveStatus::Saved | SaveStatus::Unchanged => Ok(()),
    }
}

#[cfg(test)]
#[path = "frontend_preferences_tests.rs"]
mod tests;

#[derive(Debug, thiserror::Error)]
#[error("{primary}; frontend cleanup also failed: {secondary}")]
struct ExitErrors {
    #[source]
    primary: TuiError,
    secondary: TuiError,
}

pub(super) fn exit_outcome(
    outcome: Result<(), TuiError>,
    saves: Result<(), TuiError>,
    exports: Result<(), TuiError>,
) -> Result<(), TuiError> {
    fn combine(first: Result<(), TuiError>, second: Result<(), TuiError>) -> Result<(), TuiError> {
        match (first, second) {
            (Ok(()), result) | (result, Ok(())) => result,
            (Err(primary), Err(secondary)) => Err(interaction(ExitErrors { primary, secondary })),
        }
    }
    combine(combine(outcome, saves), exports)
}
