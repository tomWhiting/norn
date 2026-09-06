//! Save-owner tests cover real local publication, in-flight edits and unknown outcomes.

use super::*;
use crate::frontend_preferences::FrontendPreferencesLaunch;
use crate::input::history::InputHistory;
use crate::render::fixed_panel::StatusBar;
use crate::terminal::caps::TerminalCaps;
use norn::config::TuiPreferenceScope;
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn state() -> AppState {
    AppState::new(
        TerminalCaps::baseline(),
        InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        super::super::state::test_view_source(uuid::Uuid::new_v4()),
        StatusBar::default(),
    )
}
fn local_launch(
    root: &std::path::Path,
) -> Result<FrontendPreferencesLaunch, norn::config::TuiPreferencesError> {
    let mut launch = FrontendPreferencesLaunch::run_only();
    launch.local = Some(TuiPreferencesSnapshot::from_layer(
        TuiPreferenceScope::WorkspaceLocal,
        root,
        None,
    )?);
    launch.scope = PreferenceScope::Local;
    Ok(launch)
}

#[test]
fn installing_preferences_changes_no_draft_pending_input_or_source() -> TestResult {
    let mut state = state();
    super::super::event_loop::insert_paste_text(&mut state, "unfinished draft");
    state
        .in_flight_input
        .queue_followup("accepted queued message".to_owned());
    let source = state.transcript.projection.source().clone();
    let mut launch = FrontendPreferencesLaunch::run_only();
    launch.initial = FrontendPreferences::decode(Some(
        &json!({"view":{"changes_open":true,"split":{"conversation":4,"changes":3}},"input":{"submit_mode":"queue"}}),
    ))?;
    install(&mut state, launch);
    assert!(state.screen.changes_open);
    assert_eq!(state.screen.split.weights(), (4, 3));
    assert_eq!(state.in_flight_input.mode().label(), "queue");
    assert_eq!(
        state.in_flight_input.pop_queued_followup().as_deref(),
        Some("accepted queued message")
    );
    assert_eq!(state.input_editor.text(), "unfinished draft");
    assert_eq!(state.transcript.projection.source(), &source);
    assert!(state.preferences.pending.is_none());
    Ok(())
}

#[tokio::test]
async fn one_pending_save_is_observed_before_latest_edits_are_published() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let mut state = state();
    install(&mut state, local_launch(&root)?);
    state.screen.changes_open = true;
    edited(&mut state)?;
    assert!(state.preferences.pending.is_some());
    state
        .in_flight_input
        .set_mode(crate::app::active_input::InFlightSubmitMode::Queue);
    edited(&mut state)?;
    assert_eq!(
        state
            .preferences
            .pending
            .as_ref()
            .ok_or("pending save absent")?
            .requested
            .submit_mode
            .label(),
        "steer"
    );
    assert!(!state.preferences.start(true)?);
    assert!(state.preferences.dirty);
    super::super::event_loop::insert_paste_text(&mut state, "typing stays available");
    drain(&mut state).await?;
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(
        root.join(".norn/settings.local.json"),
    )?)?;
    assert_eq!(saved["tui"]["view"]["changes_open"], true);
    assert_eq!(saved["tui"]["input"]["submit_mode"], "queue");
    assert_eq!(state.input_editor.text(), "typing stays available");
    assert!(!state.preferences.dirty);
    assert!(state.preferences.pending.is_none());
    assert_eq!(
        state.transcript.projection.items().len(),
        0,
        "ordinary saves add no conversation rows"
    );
    Ok(())
}

#[tokio::test]
async fn temporary_scope_preserves_an_already_started_write_without_saving_new_edits() -> TestResult
{
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let mut state = state();
    install(&mut state, local_launch(&root)?);
    state.screen.changes_open = true;
    edited(&mut state)?;
    state.preferences.scope = PreferenceScope::Run;
    state.screen.changes_open = false;
    edited(&mut state)?;
    drain(&mut state).await?;
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(
        root.join(".norn/settings.local.json"),
    )?)?;
    assert_eq!(saved["tui"]["view"]["changes_open"], true);
    assert!(!state.screen.changes_open);
    assert!(state.preferences.dirty);
    assert!(state.preferences.summary().contains("Run"));
    Ok(())
}

#[tokio::test]
async fn stale_owned_setting_preserves_run_values_and_blocks_automatic_retry() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let mut state = state();
    install(&mut state, local_launch(&root)?);
    std::fs::create_dir(root.join(".norn"))?;
    std::fs::write(
        root.join(".norn/settings.local.json"),
        "{\"tui\":{\"view\":{\"history_events\":999}}}",
    )?;
    state.screen.changes_open = true;
    edited(&mut state)?;
    let result = wait(&mut state.preferences).await;
    finish(&mut state, result)?;
    assert!(state.preferences.blocked);
    assert!(state.preferences.dirty);
    assert!(state.screen.changes_open);
    state.display_toggles.toggle();
    edited(&mut state)?;
    assert!(state.preferences.pending.is_none());
    assert!(state.preferences.summary().contains("tui.view"));
    assert!(drain(&mut state).await.is_err());
    Ok(())
}

#[tokio::test]
async fn task_failure_is_unknown_publication_and_cannot_be_blindly_retried() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let mut state = state();
    install(&mut state, local_launch(&root)?);
    let task = tokio::spawn(std::future::pending());
    task.abort();
    state.preferences.pending = Some(PendingSave {
        scope: PreferenceScope::Local,
        requested: capture(&state),
        path: root.join(".norn/settings.local.json"),
        task,
    });
    let result = wait(&mut state.preferences).await;
    finish(&mut state, result)?;
    assert!(state.preferences.outcome_unknown);
    assert!(
        state
            .preferences
            .summary()
            .contains("publication may have happened")
    );
    assert!(state.preferences.start(true).is_err());
    assert!(state.preferences.pending.is_none());
    Ok(())
}
