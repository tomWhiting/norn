//! TUI-owned read-aloud operations and controls; no provider input or audio polling.

use norn::integration::locutus::{
    ReadAloudOutcome, ReadAloudProgress, ReadAloudRequest, read_aloud,
};
use norn::session_view::ViewSource;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{notices, render::interaction, slash::LocalCommandOutcome, state::AppState};
use crate::TuiError;
use crate::voice_preferences::VoicePreferences;

type PlaybackResult = Result<ReadAloudOutcome, norn::integration::locutus::ReadAloudError>;
type JoinedPlayback = Result<PlaybackResult, tokio::task::JoinError>;

#[derive(Clone)]
struct Answer {
    source: ViewSource,
    text: String,
}

struct Playback {
    request_id: uuid::Uuid,
    cancel: CancellationToken,
    progress: watch::Receiver<ReadAloudProgress>,
    task: JoinHandle<PlaybackResult>,
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// No connection, channel or task exists until an actual read request is admitted.
#[derive(Default)]
pub(super) struct VoiceOwner {
    latest: Option<Answer>,
    replay: Option<Answer>,
    playback: Option<Playback>,
}

pub(super) enum VoiceUpdate {
    Progress(ReadAloudProgress),
    Finished(JoinedPlayback),
}

pub(super) async fn wait(owner: &mut VoiceOwner) -> VoiceUpdate {
    let Some(playback) = owner.playback.as_mut() else {
        return std::future::pending().await;
    };
    tokio::select! {
        result = &mut playback.task => VoiceUpdate::Finished(result),
        changed = playback.progress.changed() => {
            if changed.is_ok() { VoiceUpdate::Progress(playback.progress.borrow_and_update().clone()) }
            else { VoiceUpdate::Finished((&mut playback.task).await) }
        }
    }
}

pub(super) fn finish(state: &mut AppState, update: VoiceUpdate) -> Result<(), TuiError> {
    match update {
        VoiceUpdate::Progress(progress) => {
            state.screen.feedback = Some(match progress {
                ReadAloudProgress::Connecting => "Voice: connecting".to_owned(),
                ReadAloudProgress::Submitted => "Voice: awaiting admission".to_owned(),
                ReadAloudProgress::Accepted { id } => {
                    format!("Voice {id}: accepted; waiting for playback")
                }
                ReadAloudProgress::Playing { id } => format!("Voice {id}: playing"),
                ReadAloudProgress::Stopping => "Voice: stopping; awaiting confirmation".to_owned(),
            });
        }
        VoiceUpdate::Finished(result) => {
            let Some(playback) = state.voice.playback.take() else {
                return Err(interaction(std::io::Error::other(
                    "voice completion has no owned request",
                )));
            };
            let receipt = match result {
                Ok(Ok(ReadAloudOutcome::NotSubmitted)) => "cancelled before submission".to_owned(),
                Ok(Ok(ReadAloudOutcome::Completed {
                    duration_ms,
                    stop_requested,
                    ..
                })) => {
                    if stop_requested {
                        format!(
                            "server reports completed playback · {duration_ms} ms · stop was requested"
                        )
                    } else {
                        format!("completed · {duration_ms} ms")
                    }
                }
                Ok(Ok(ReadAloudOutcome::StopUnconfirmed { stop_sent, waited })) => {
                    let action = if stop_sent {
                        "stop sent"
                    } else {
                        "stop requested; hush write unconfirmed"
                    };
                    format!(
                        "{action}; no receipt within {} ms; playback outcome unknown",
                        waited.as_millis()
                    )
                }
                Ok(Ok(ReadAloudOutcome::Stopped {
                    at_ms, latency_ms, ..
                })) => format!(
                    "stopped · callback position {at_ms} ms · device latency {latency_ms} ms"
                ),
                Ok(Err(error)) => {
                    notices::error(state, "Read-aloud failed", &error.to_string())?;
                    state.screen.feedback =
                        Some("Voice failed; inspect the retained error".to_owned());
                    state.screen.dirty = true;
                    return Ok(());
                }
                Err(error) => return Err(interaction(error)),
            };
            notices::notice(
                state,
                &format!("Voice {}: {receipt}", playback.request_id),
                None,
            )?;
            state.screen.feedback = Some(format!("Voice: {receipt}"));
        }
    }
    state.screen.dirty = true;
    Ok(())
}

/// Retain only a completed textual answer. Structured contracts need an explicit
/// spoken projection; raw JSON and reasoning are never selected for speech.
pub(super) fn completed(state: &mut AppState, output: &serde_json::Value) -> Result<(), TuiError> {
    state.voice.latest = output
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(|text| Answer {
            source: state.transcript.projection.source().clone(),
            text: text.to_owned(),
        });
    if state.voice_preferences.enabled
        && state.voice_preferences.automatic
        && let Err(error) = start(state, false)
    {
        notices::error(
            state,
            "Automatic read-aloud not started",
            &error.to_string(),
        )?;
    }
    Ok(())
}

fn start(state: &mut AppState, replay: bool) -> Result<(), TuiError> {
    let refuse = |text: &str| interaction(std::io::Error::other(text.to_owned()));
    if !state.voice_preferences.enabled {
        return Err(refuse(
            "voice is off; configure the native socket and use /voice on",
        ));
    }
    if state.voice.playback.is_some() {
        return Err(refuse(
            "read-aloud is busy; /voice stop then wait for its receipt",
        ));
    }
    let answer = if replay {
        state.voice.replay.as_ref()
    } else {
        state.voice.latest.as_ref()
    }
    .ok_or_else(|| refuse("no completed textual answer is available for read-aloud"))?
    .clone();
    if &answer.source != state.transcript.projection.source() {
        return Err(refuse(
            "the selected answer belongs to a different session or agent",
        ));
    }
    state.voice_preferences.validate().map_err(interaction)?;
    let socket = state
        .voice_preferences
        .control_socket
        .clone()
        .ok_or_else(|| refuse("voice has no control socket"))?;
    let request = ReadAloudRequest {
        id: uuid::Uuid::new_v4(),
        seat: format!("norn-{}", answer.source.agent_id),
        socket,
        text: answer.text.clone(),
        voice: state.voice_preferences.voice.clone(),
    };
    let request_id = request.id;
    let cancel = CancellationToken::new();
    let (progress, receiver) = watch::channel(ReadAloudProgress::Connecting);
    let task = tokio::spawn(read_aloud(request, cancel.clone(), progress));
    state.voice.playback = Some(Playback {
        request_id,
        cancel,
        progress: receiver,
        task,
    });
    state.voice.replay = Some(answer);
    state.screen.feedback = Some("Voice: connecting".to_owned());
    Ok(())
}

fn stop(owner: &VoiceOwner) {
    if let Some(playback) = &owner.playback {
        playback.cancel.cancel();
    }
}

pub(super) const HELP: &str = "Native voice\n/voice configure <JSON> · {\"control_socket\":\"/absolute/path/locutus.sock\",\"enabled\":true,\"automatic\":false,\"voice\":null}\n/voice on|off · enable native read-aloud; off also stops current playback\n/voice read · read the latest completed text answer\n/voice replay · read the last selected answer again\n/voice stop · stop only this terminal's speech; agent work continues\n/voice auto on|off · read newly completed answers automatically\n/voice status · show native voice settings\nPreferences use /view preferences run|user|local|save. Hub admission still applies: accepted speech may wait for Play in Dot.";

pub(super) fn command(text: &str, state: &mut AppState) -> Result<LocalCommandOutcome, TuiError> {
    let result = execute(text.trim(), state);
    state.screen.dirty = true;
    match result {
        Ok(()) => match super::frontend_preferences::edited(state) {
            Ok(()) => Ok(LocalCommandOutcome::Accepted),
            Err(error) => Ok(LocalCommandOutcome::after_reported_failure(error, Ok(()))),
        },
        Err(error) => {
            notices::error(state, "Voice command", &error.to_string())?;
            Ok(LocalCommandOutcome::Rejected)
        }
    }
}

fn execute(text: &str, state: &mut AppState) -> Result<(), TuiError> {
    match text {
        "" | "help" => {
            notices::notice(state, HELP, None)?;
        }
        "read" => start(state, false)?,
        "replay" => start(state, true)?,
        "stop" => {
            stop(&state.voice);
            state.screen.feedback = Some(
                if state.voice.playback.is_some() {
                    "Voice: stop requested"
                } else {
                    "Voice: idle"
                }
                .to_owned(),
            );
        }
        "off" => {
            state.voice_preferences.enabled = false;
            stop(&state.voice);
        }
        "on" => {
            let mut preferences = state.voice_preferences.clone();
            preferences.enabled = true;
            preferences.validate().map_err(interaction)?;
            state.voice_preferences = preferences;
        }
        "auto on" => state.voice_preferences.automatic = true,
        "auto off" => state.voice_preferences.automatic = false,
        "status" => {
            notices::notice(
                state,
                &state.voice_preferences.projection().to_string(),
                None,
            )?;
        }
        _ => {
            let Some(json) = text.strip_prefix("configure ") else {
                return Err(interaction(std::io::Error::other(
                    "use /voice help for available controls",
                )));
            };
            if state.voice.playback.is_some() {
                return Err(interaction(std::io::Error::other(
                    "stop playback and wait for its receipt before changing the voice destination",
                )));
            }
            let value = serde_json::from_str(json).map_err(interaction)?;
            state.voice_preferences =
                VoicePreferences::decode(Some(&value)).map_err(interaction)?;
        }
    }
    Ok(())
}

pub(super) async fn drain(state: &mut AppState) -> Result<(), TuiError> {
    stop(&state.voice);
    while state.voice.playback.is_some() {
        let update = wait(&mut state.voice).await;
        finish(state, update)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "voice_tests.rs"]
mod tests;
