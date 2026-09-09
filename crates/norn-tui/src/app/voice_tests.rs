//! Voice controls stay local, preserve drafts, and refuse stale session selections.

use super::*;
use crate::frontend_preferences::FrontendPreferencesLaunch;
use crate::input::history::InputHistory;
use crate::render::fixed_panel::StatusBar;
use crate::terminal::caps::TerminalCaps;

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

#[test]
fn completed_answers_do_not_start_disabled_voice() -> TestResult {
    let mut state = state();
    completed(&mut state, &serde_json::json!("A completed answer."))?;
    assert!(state.voice.latest.is_some());
    assert!(state.voice.playback.is_none());
    assert!(matches!(
        command("read", &mut state)?,
        LocalCommandOutcome::Rejected
    ));
    assert!(state.voice.playback.is_none());
    Ok(())
}

#[test]
fn structured_output_is_never_silently_read_as_json() -> TestResult {
    let mut state = state();
    completed(&mut state, &serde_json::json!("Earlier answer."))?;
    completed(
        &mut state,
        &serde_json::json!({"private_field":"not a spoken projection"}),
    )?;
    assert!(state.voice.latest.is_none());
    assert!(state.voice.playback.is_none());
    Ok(())
}

#[test]
fn configuring_voice_changes_no_draft_and_opens_no_connection() -> TestResult {
    let mut state = state();
    super::super::frontend_preferences::install(&mut state, FrontendPreferencesLaunch::run_only());
    super::super::event_loop::insert_paste_text(&mut state, "unfinished draft")?;
    let source = state.transcript.projection.source().clone();
    assert!(matches!(
        command(
            "configure {\"enabled\":true,\"control_socket\":\"/a/not-running/socket\",\"automatic\":false}",
            &mut state
        )?,
        LocalCommandOutcome::Accepted
    ));
    assert!(state.voice_preferences.enabled);
    assert!(state.voice.playback.is_none());
    assert_eq!(state.input_editor.text(), "unfinished draft");
    assert_eq!(state.transcript.projection.source(), &source);
    let before = state.voice_preferences.clone();
    assert!(matches!(
        command("configure {\"enabled\":true}", &mut state)?,
        LocalCommandOutcome::Rejected
    ));
    assert_eq!(state.voice_preferences, before);
    Ok(())
}

#[test]
fn answer_from_another_session_cannot_be_replayed_in_this_one() -> TestResult {
    let mut state = state();
    state.voice_preferences = VoicePreferences::decode(Some(
        &serde_json::json!({"enabled":true,"control_socket":"/not-connected"}),
    ))?;
    let other = super::super::state::test_view_source(uuid::Uuid::new_v4());
    state.voice.replay = Some(Answer {
        source: other,
        text: "An answer from another session.".to_owned(),
    });
    assert!(matches!(
        command("replay", &mut state)?,
        LocalCommandOutcome::Rejected
    ));
    assert!(state.voice.playback.is_none());
    Ok(())
}

#[test]
fn voice_commands_are_frontend_commands_while_the_agent_is_running() {
    for text in ["/voice read", "/VOICE stop", "/voice configure {}"] {
        assert!(super::super::view_actions::is_frontend_command(text));
    }
    assert!(!super::super::view_actions::is_frontend_command(
        "please /voice read"
    ));
}
