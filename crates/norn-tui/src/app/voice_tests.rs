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

#[tokio::test]
async fn connect_failure_becomes_a_retained_error_and_releases_playback() -> TestResult {
    let directory = tempfile::tempdir()?;
    let mut state = state();
    state.voice_preferences = VoicePreferences::decode(Some(&serde_json::json!({
        "enabled":true,"control_socket":directory.path().join("no-listener")
    })))?;
    completed(&mut state, &serde_json::json!("Read this answer."))?;
    assert!(matches!(
        command("read", &mut state)?,
        LocalCommandOutcome::Accepted
    ));
    while state.voice.playback.is_some() {
        let update = wait(&mut state.voice).await;
        finish(&mut state, update)?;
    }
    assert!(state.transcript.projection.items().any(|item| matches!(
        item.kind,
        norn::session_view::ViewItemKind::Error
    ) && item.label.as_str()
        == "Read-aloud failed"));
    assert_eq!(
        state.screen.feedback.as_deref(),
        Some("Voice failed; inspect the retained error")
    );
    Ok(())
}

#[tokio::test]
async fn busy_controls_cannot_replace_the_owned_request() -> TestResult {
    let mut state = state();
    state.voice_preferences = VoicePreferences::decode(Some(&serde_json::json!({
        "enabled":true,"control_socket":"/not-opened"
    })))?;
    completed(&mut state, &serde_json::json!("The answer."))?;
    let request_id = uuid::Uuid::new_v4();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let (progress, receiver) = watch::channel(ReadAloudProgress::Stopping);
    state.voice.playback = Some(Playback {
        request_id,
        cancel,
        progress: receiver,
        task: tokio::spawn(async move {
            task_cancel.cancelled().await;
            Ok(ReadAloudOutcome::StopUnconfirmed {
                stop_sent: true,
                waited: std::time::Duration::from_secs(5),
            })
        }),
    });
    for text in ["read", "replay", "configure {}"] {
        assert!(matches!(
            command(text, &mut state)?,
            LocalCommandOutcome::Rejected
        ));
        assert_eq!(
            state
                .voice
                .playback
                .as_ref()
                .ok_or("lost playback owner")?
                .request_id,
            request_id
        );
    }
    drain(&mut state).await?;
    assert!(state.voice.playback.is_none());
    assert!(state.transcript.projection.items().any(|item| {
        item.label
            .as_str()
            .contains("stop sent; no receipt within 5000 ms; playback outcome unknown")
    }));
    drop(progress);
    Ok(())
}

#[tokio::test]
async fn completion_notice_retains_a_late_stop_request() -> TestResult {
    let mut state = state();
    let (progress, receiver) = watch::channel(ReadAloudProgress::Stopping);
    state.voice.playback = Some(Playback {
        request_id: uuid::Uuid::new_v4(),
        cancel: CancellationToken::new(),
        progress: receiver,
        task: tokio::spawn(async {
            Ok(ReadAloudOutcome::Completed {
                session: "voice-session".to_owned(),
                id: 41,
                duration_ms: 2500,
                stop_requested: true,
            })
        }),
    });
    while state.voice.playback.is_some() {
        let update = wait(&mut state.voice).await;
        finish(&mut state, update)?;
    }
    assert!(
        state
            .screen
            .feedback
            .as_deref()
            .is_some_and(|text| text.contains("server reports completed playback")
                && text.contains("stop was requested"))
    );
    drop(progress);
    Ok(())
}
