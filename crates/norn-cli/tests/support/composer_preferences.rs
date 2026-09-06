//! Composer gestures through the existing real CLI PTY owner; no alternate process harness.

use std::io;

use serde_json::Value;

use crate::restart_support::{App, TestResult};
use crate::retained_screen::Screen;

/// Explicit fixture send policy, matching the settings under test.
#[derive(Clone, Copy)]
pub enum SendKey {
    /// Enter submits; a reported Alt+Enter inserts a newline.
    Enter,
    /// A reported Shift+Enter submits; Enter inserts a newline.
    ShiftEnter,
    /// A reported Alt+Enter submits; Enter inserts a newline.
    AltEnter,
}

impl SendKey {
    fn submit(self) -> &'static [u8] {
        match self {
            Self::Enter => b"\r",
            Self::ShiftEnter => b"\x1b[13;2u",
            Self::AltEnter => b"\x1b[13;3u",
        }
    }

    fn newline(self) -> &'static [u8] {
        match self {
            Self::Enter => b"\x1b[13;3u",
            Self::ShiftEnter | Self::AltEnter => b"\r",
        }
    }
}

fn composer_text(screen: &Screen) -> String {
    let lines = screen.lines();
    screen
        .composer_rows()
        .into_iter()
        .filter_map(|row| lines.get(row))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

fn draft(app: &mut App, text: &str) -> TestResult<Screen> {
    let before = app.frame(0, |_| true)?;
    // A single original Paste event is not a submit, even when it contains a slash command.
    app.press(format!("\x1b[200~{text}\x1b[201~").as_bytes())?;
    Ok(app.frame(before.end_offset, |screen| composer_text(screen) == text)?)
}

/// Submit one local command using the active physical policy and wait beyond its draft frame.
pub fn observe(app: &mut App, send: SendKey, command: &str, expected: &str) -> TestResult<Screen> {
    let entered = draft(app, command)?;
    app.press(send.submit())?;
    Ok(app.frame(entered.end_offset, |screen| {
        composer_text(screen).is_empty() && screen.contains(expected)
    })?)
}

/// Prove the non-send key inserts one hard newline and the send key admits the whole draft.
/// Request census later checks exact original bytes, so an early first-line submission fails.
pub fn submit_multiline(app: &mut App, send: SendKey, first: &str, second: &str) -> TestResult {
    let first_frame = draft(app, first)?;
    app.press(send.newline())?;
    let newline = format!("{first}\n");
    let newline_frame = app.frame(first_frame.end_offset, |screen| {
        composer_text(screen) == newline
    })?;
    newline_frame.assert_composer(2)?;
    let text = format!("{first}\n{second}");
    app.press(second.as_bytes())?;
    let complete = app.frame(newline_frame.end_offset, |screen| {
        composer_text(screen) == text
    })?;
    app.press(send.submit())?;
    let response = app.frame(complete.end_offset, |screen| {
        composer_text(screen).is_empty()
            && screen.contains("restart fixture answer")
            && screen.contains("Turn completed")
            && screen.contains(first)
            && screen.contains(second)
    })?;
    response.assert_composer(1)?;
    Ok(())
}

/// Assert every actual local HTTP request contains exactly its intended original user draft.
/// Local settings commands and newline-only gestures must never add requests.
pub fn assert_requests(requests: &[Value], prompts: &[&str]) -> TestResult {
    assert_eq!(
        requests.len(),
        prompts.len(),
        "local actions or newline keys admitted extra work"
    );
    for (request, prompt) in requests.iter().zip(prompts) {
        let messages = request["messages"]
            .as_array()
            .ok_or_else(|| io::Error::other("actual Chat request lacks messages"))?;
        let users: Vec<&Value> = messages
            .iter()
            .filter(|message| message["role"] == "user")
            .collect();
        assert_eq!(
            users.len(),
            1,
            "fresh CLI request contains unexpected user inputs"
        );
        let user = users
            .first()
            .ok_or_else(|| io::Error::other("actual Chat request lacks user input"))?;
        match &user["content"] {
            Value::String(text) => assert_eq!(text, *prompt),
            Value::Array(parts) => {
                assert_eq!(parts.len(), 1, "original text was split or supplemented");
                let part = parts
                    .first()
                    .ok_or_else(|| io::Error::other("actual user content is empty"))?;
                assert_eq!(part["type"], "text");
                assert_eq!(part["text"], *prompt);
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => {
                return Err(
                    io::Error::other("actual user content is neither text nor text parts").into(),
                );
            }
        }
    }
    Ok(())
}
