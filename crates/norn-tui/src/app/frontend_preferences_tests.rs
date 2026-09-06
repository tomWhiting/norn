//! Save-owner tests cover real local publication, in-flight edits and unknown outcomes.

use super::*;
use crate::frontend_preferences::{ComposerSendKey, FrontendPreferencesLaunch};
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
    super::super::event_loop::insert_paste_text(&mut state, "unfinished draft")?;
    state
        .in_flight_input
        .queue_followup("accepted queued message".to_owned());
    let source = state.transcript.projection.source().clone();
    let mut launch = FrontendPreferencesLaunch::run_only();
    launch.initial = FrontendPreferences::decode(Some(
        &json!({"view":{"changes_open":true,"split":{"conversation":4,"changes":3}},"input":{"submit_mode":"queue"},"composer":{"send_key":"alt-enter"}}),
    ))?;
    install(&mut state, launch);
    assert!(state.screen.changes_open);
    assert_eq!(state.screen.split.weights(), (4, 3));
    assert_eq!(state.in_flight_input.mode().label(), "queue");
    assert_eq!(state.composer_send_key, ComposerSendKey::AltEnter);
    assert_eq!(capture(&state), state.preferences.current);
    assert_eq!(
        state.in_flight_input.pop_queued_followup().as_deref(),
        Some("accepted queued message")
    );
    assert_eq!(state.input_editor.text(), "unfinished draft");
    assert_eq!(state.transcript.projection.source(), &source);
    assert!(state.preferences.pending.is_none());
    Ok(())
}

#[test]
fn composer_commands_change_only_the_send_policy_and_reject_unknown_keys() -> TestResult {
    let mut state = state();
    assert_eq!(state.composer_send_key, ComposerSendKey::Enter);
    super::super::event_loop::insert_paste_text(&mut state, "preserved draft")?;
    state
        .in_flight_input
        .queue_followup("accepted followup".to_owned());
    let source = state.transcript.projection.source().clone();
    for (command, expected) in [
        ("composer send-key shift-enter", ComposerSendKey::ShiftEnter),
        ("composer send-key alt-enter", ComposerSendKey::AltEnter),
        ("composer send-key enter", ComposerSendKey::Enter),
    ] {
        super::super::view_actions::command(command, &mut state)?;
        assert_eq!(state.composer_send_key, expected);
        assert_eq!(capture(&state).composer_send_key, expected);
        assert_eq!(state.in_flight_input.mode().label(), "steer");
        assert!(state.preferences.pending.is_none());
        assert_eq!(state.transcript.projection.items().len(), 0);
    }
    let before = capture(&state);
    super::super::view_actions::command("composer send-key unsupported", &mut state)?;
    assert_eq!(capture(&state), before);
    assert_eq!(state.input_editor.text(), "preserved draft");
    assert_eq!(
        state.in_flight_input.pop_queued_followup().as_deref(),
        Some("accepted followup")
    );
    assert_eq!(state.transcript.projection.source(), &source);
    assert!(state.preferences.pending.is_none());
    assert_eq!(
        state.transcript.projection.items().len(),
        1,
        "invalid command creates a local error row"
    );
    Ok(())
}

#[tokio::test]
async fn composer_save_preserves_unowned_data_and_reinstalls_the_published_policy() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let path = root.join(".norn/settings.local.json");
    let original =
        json!({"env":{"sentinel":"unchanged"},"tui":{"extension_data":{"nested":[1,true,null]}}});
    std::fs::create_dir(root.join(".norn"))?;
    std::fs::write(&path, serde_json::to_vec(&original)?)?;
    let mut launch = local_launch(&root)?;
    launch.local = Some(TuiPreferencesSnapshot::from_layer(
        TuiPreferenceScope::WorkspaceLocal,
        &root,
        original.get("tui").cloned(),
    )?);
    let mut active = state();
    install(&mut active, launch);
    super::super::view_actions::command("composer send-key shift-enter", &mut active)?;
    assert!(active.preferences.pending.is_some());
    drain(&mut active).await?;
    let saved: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    assert_eq!(saved["env"], original["env"]);
    assert_eq!(
        saved["tui"]["extension_data"],
        original["tui"]["extension_data"]
    );
    assert_eq!(saved["tui"]["composer"], json!({"send_key":"shift-enter"}));
    let mut restored = state();
    let mut launch = FrontendPreferencesLaunch::run_only();
    launch.initial = FrontendPreferences::decode(saved.get("tui"))?;
    install(&mut restored, launch);
    assert_eq!(restored.composer_send_key, ComposerSendKey::ShiftEnter);
    assert_eq!(capture(&restored), capture(&active));
    assert!(restored.preferences.pending.is_none());
    assert_eq!(active.transcript.projection.items().len(), 0);
    Ok(())
}

#[tokio::test]
async fn concurrent_composer_edit_is_an_owned_conflict_without_overwriting_external_bytes()
-> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let mut state = state();
    install(&mut state, local_launch(&root)?);
    std::fs::create_dir(root.join(".norn"))?;
    let path = root.join(".norn/settings.local.json");
    let external = br#"{"tui":{"composer":{"send_key":"enter"}}}"#;
    std::fs::write(&path, external)?;
    super::super::view_actions::command("composer send-key alt-enter", &mut state)?;
    let outcome = wait(&mut state.preferences).await;
    finish(&mut state, outcome)?;
    assert_eq!(state.composer_send_key, ComposerSendKey::AltEnter);
    assert_eq!(std::fs::read(&path)?.as_slice(), external);
    assert!(state.preferences.blocked);
    assert!(state.preferences.dirty);
    assert!(state.preferences.pending.is_none());
    assert!(state.preferences.summary().contains("tui.composer"));
    assert!(drain(&mut state).await.is_err());
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
    state.composer_send_key = ComposerSendKey::ShiftEnter;
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
    assert_eq!(
        state
            .preferences
            .pending
            .as_ref()
            .ok_or("pending save absent")?
            .requested
            .composer_send_key,
        ComposerSendKey::Enter
    );
    assert!(state.preferences.dirty);
    super::super::event_loop::insert_paste_text(&mut state, "typing stays available")?;
    drain(&mut state).await?;
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(
        root.join(".norn/settings.local.json"),
    )?)?;
    assert_eq!(saved["tui"]["view"]["changes_open"], true);
    assert_eq!(saved["tui"]["input"]["submit_mode"], "queue");
    assert_eq!(saved["tui"]["composer"]["send_key"], "shift-enter");
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

#[tokio::test]
async fn shortcut_edits_publish_and_restore_without_changing_draft_or_unowned_data() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let mut active = state();
    install(&mut active, local_launch(&root)?);
    super::super::event_loop::insert_paste_text(&mut active, "kept🙂")?;
    let draft = active.input_editor.snapshot()?;
    super::super::view_actions::command("keys set pane_toggle alt+q f7", &mut active)?;
    active.input_editor.validate_snapshot(&draft)?;
    drain(&mut active).await?;
    let saved: Value =
        serde_json::from_slice(&std::fs::read(root.join(".norn/settings.local.json"))?)?;
    assert_eq!(
        saved["tui"]["input"]["bindings"]["pane_toggle"],
        json!(["alt+q", "f7"])
    );
    let mut restored = state();
    let mut launch = FrontendPreferencesLaunch::run_only();
    launch.initial = FrontendPreferences::decode(saved.get("tui"))?;
    install(&mut restored, launch);
    assert_eq!(restored.view_shortcuts, active.view_shortcuts);
    let original = Arc::clone(&active.view_shortcuts);
    super::super::view_actions::command("keys set pane_toggle ctrl+z", &mut active)?;
    assert!(Arc::ptr_eq(&original, &active.view_shortcuts));
    active.input_editor.validate_snapshot(&draft)?;
    assert!(active.preferences.pending.is_none());
    Ok(())
}
