//! Shared helpers used by the dispatch and event-loop layers.
//!
//! Extracted from `dispatch.rs` to keep the dispatch module under the
//! 500-line production code limit. Contains dim-stream terminal
//! helpers, ANSI-aware width measurement, usage formatting, and tool
//! argument extraction.

use std::io::Write as _;
use std::time::Duration;

use serde_json::Value;

use norn::provider::usage::Usage;
use norn::tool::split_envelope_fields;

use crate::TuiError;
use crate::render::MarkdownRenderer;
use crate::render::scroll_region::write_to_scroll;
use crate::render::style::sync_markers;
use crate::render::text::format_count;
use crate::terminal::caps::TerminalCaps;
use crate::terminal::setup::TerminalGuard;

use super::state::{AppState, PendingToolCall};
use super::tool_calls::{parse_args, write_tool_result};

/// Break the dim-stream cycle before a non-markdown-stream write.
///
/// Called at the top of [`crate::app::tool_calls::write_tool_result`],
/// [`flush_markdown`], and [`flush_pending`] to fix the dim-to-tool
/// transition bug: without this, a `TextDelta("Here is my")` followed
/// by a `ToolResult` leaves the dim preview text on screen mixed with
/// tool output, and the next tick handler's erase-then-repaint pass
/// destroys real content because `state.dim_wrapped_lines` still says
/// "1 dim row above the cursor" when those rows now hold tool output.
///
/// Erases the dim preview rows from the scroll region (gated on
/// [`MarkdownRenderer::clear_dim`] reporting that dim was actually
/// live — calling [`erase_dim_lines`] when no dim was painted would
/// destroy real content), then zeros [`AppState::dim_wrapped_lines`]
/// and [`AppState::styled_mid_line`] defensively so subsequent writes
/// start on a clean line at column 1.
///
/// The renderer's pending markdown buffer is intentionally left
/// untouched — the next `feed` or `finalize` can still drain it. This
/// separates terminal-state reset from parser-state reset.
pub(crate) fn clear_dim_state(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: Option<&mut MarkdownRenderer>,
) -> Result<(), TuiError> {
    let was_active = renderer.is_some_and(MarkdownRenderer::clear_dim);
    if was_active && state.dim_wrapped_lines > 0 {
        erase_dim_lines(state.dim_wrapped_lines, guard)?;
    }
    state.dim_wrapped_lines = 0;
    state.styled_mid_line = false;
    Ok(())
}

/// Flush remaining markdown content without touching pending tool calls.
///
/// Used by [`crate::app::dispatch::handle_done`] where pending tools
/// are expected to receive their `ToolResult` events shortly — flushing
/// them here would render null output prematurely.
///
/// Calls [`clear_dim_state`] first so any live dim preview is erased
/// before the styled tail is written, preventing the dim-to-tool
/// transition bug from also corrupting the end-of-turn flush.
pub fn flush_markdown(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
) -> Result<(), TuiError> {
    clear_dim_state(state, guard, renderer.as_mut())?;
    if let Some(r) = renderer.as_mut() {
        let output = r.finalize();
        if output.replace_dim {
            erase_dim_lines(state.dim_wrapped_lines, guard)?;
            state.dim_wrapped_lines = 0;
        }
        if !output.styled.is_empty() {
            write_to_scroll(&output.styled, guard.terminal_mut())?;
            guard.note_scroll_newlines(&output.styled)?;
            state.text_streamed_this_turn = true;
        }
    }
    *renderer = None;
    flush_terminal(guard)
}

/// Flush remaining markdown content and any unresolved pending tool
/// calls into the scroll region. Pending tool calls without a
/// `ToolResult` use [`Value::Null`] as the result (the brief allows
/// this "accumulated deltas serve as the args fallback" path).
///
/// Calls [`clear_dim_state`] first so the dim preview is erased before
/// the unresolved tool rows render — without this, a partial-stream
/// error path would leak ghost dim text into the final tool output.
pub fn flush_pending(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
) -> Result<(), TuiError> {
    clear_dim_state(state, guard, renderer.as_mut())?;
    if let Some(r) = renderer.as_mut() {
        let output = r.finalize();
        if output.replace_dim {
            erase_dim_lines(state.dim_wrapped_lines, guard)?;
            state.dim_wrapped_lines = 0;
        }
        if !output.styled.is_empty() {
            write_to_scroll(&output.styled, guard.terminal_mut())?;
            guard.note_scroll_newlines(&output.styled)?;
            state.text_streamed_this_turn = true;
        }
    }
    *renderer = None;
    let unresolved: Vec<(String, PendingToolCall)> = std::mem::take(&mut state.pending_tools)
        .into_iter()
        .collect();
    for (_id, pending) in unresolved {
        if let Some(name) = pending.name.as_deref() {
            let args = parse_args(&pending);
            write_tool_result(state, guard, renderer, name, &args, &Value::Null, 0)?;
        }
    }
    flush_terminal(guard)
}

/// Drain the terminal's write buffer to the OS.
///
/// Wraps the bare `flush` call with the `TuiError` error type so call
/// sites can use the `?` operator consistently with the rest of the
/// dispatch and helper surface.
pub(crate) fn flush_terminal(guard: &mut TerminalGuard) -> Result<(), TuiError> {
    guard.terminal_mut().flush()?;
    Ok(())
}

/// Run `body` wrapped in synchronized-update brackets that take
/// `&mut TerminalGuard` rather than `&mut W`.
///
/// `body` is the only place the guard is held mutably, so it can call
/// the standard paint helpers (`redraw_panel`, `write_to_scroll`,
/// `note_scroll_newlines`, …) directly. On capable terminals the body
/// runs between `CSI ?2026h` / `CSI ?2026l` so the redraw is presented
/// atomically; on baseline terminals the body is bracketed with cursor
/// hide / cursor show (`CSI ?25l` / `CSI ?25h`) as the established
/// flicker-suppression fallback. The fallback is byte-equivalent to
/// the existing inline hide/show pair at the streaming-tick site, so
/// no visible behaviour changes on terminals without DCS 2026 support
/// — only capable terminals upgrade to true atomic paint.
///
/// The closing escape is ALWAYS attempted, regardless of whether the
/// prefix write succeeded or `body` returned an error, so the terminal
/// cannot be left stuck in synchronized-output or hidden-cursor state
/// even when a partial-write failure split the prefix in flight. The
/// body is only invoked when the prefix succeeds — running its writes
/// without the sync bracket would defeat the flicker guarantee.
/// Error precedence: prefix > body > suffix. Mirrors the semantics of
/// [`crate::render::style::sync_render`] — that function takes
/// `&mut W` and so cannot host helpers that re-borrow the guard, which
/// is what R11 needs at the three scroll-region streaming sites.
pub(crate) fn sync_with_guard<F>(
    caps: &TerminalCaps,
    guard: &mut TerminalGuard,
    body: F,
) -> Result<(), TuiError>
where
    F: FnOnce(&mut TerminalGuard) -> Result<(), TuiError>,
{
    let (prefix, suffix) = sync_markers(caps);
    let prefix = prefix.to_string();
    let suffix = suffix.to_string();
    let prefix_attempt: Result<(), TuiError> =
        write!(guard.terminal_mut(), "{prefix}").map_err(TuiError::from);
    let result = prefix_attempt.and_then(|()| body(guard));
    let suffix_attempt: Result<(), TuiError> =
        write!(guard.terminal_mut(), "{suffix}").map_err(TuiError::from);
    result.and(suffix_attempt)
}

/// Erase all terminal lines occupied by the current dim preview.
///
/// Moves the cursor up to the first wrapped line and erases each
/// line individually with `\r\x1b[2K` rather than the aggressive
/// `\x1b[J` (erase to end of display) which would wipe any content
/// below if the line count was over-estimated. Leaves the cursor at
/// the first erased line (column 1) so the styled replacement writes
/// from the correct position.
pub(crate) fn erase_dim_lines(wrapped: u16, guard: &mut TerminalGuard) -> Result<(), TuiError> {
    let writer = guard.terminal_mut();
    if wrapped <= 1 {
        writer.write_all(b"\r\x1b[2K")?;
        return Ok(());
    }
    write!(writer, "\x1b[{}A", wrapped - 1)?;
    for i in 0..wrapped {
        writer.write_all(b"\r\x1b[2K")?;
        if i < wrapped - 1 {
            writer.write_all(b"\x1b[B")?;
        }
    }
    write!(writer, "\x1b[{}A", wrapped - 1)?;
    Ok(())
}

/// Count how many terminal lines `text` occupies at `cols` width.
///
/// Strips ANSI escape sequences before measuring so SGR markers in
/// the markdown-aware dim preview don't inflate the width calculation.
/// Each `\n` starts a new terminal line regardless of column position.
pub(crate) fn dim_line_count(text: &str, cols: u16) -> u16 {
    if text.is_empty() || cols == 0 {
        return 0;
    }
    let cols = usize::from(cols);
    let mut total_lines: usize = 0;
    for line in text.split('\n') {
        let visible = visible_width(line);
        total_lines += if visible == 0 {
            1
        } else {
            visible.div_ceil(cols)
        };
    }
    if text.ends_with('\n') && total_lines > 0 {
        total_lines -= 1;
    }
    u16::try_from(total_lines).unwrap_or(u16::MAX).max(1)
}

/// Measure the visible (display) width of `s`, skipping ANSI CSI
/// escape sequences (`\x1b[...` terminated by a letter).
fn visible_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthChar as _;
    let mut width: usize = 0;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.next() == Some('[') {
                for esc_ch in chars.by_ref() {
                    if esc_ch.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            width += ch.width().unwrap_or(0);
        }
    }
    width
}

/// Compose `[{input} in / {output} out, {elapsed}]`.
///
/// Inlined here (rather than imported from `norn-cli`) so `norn-tui` does
/// not depend on `norn-cli`.
pub fn format_usage_summary(usage: &Usage, elapsed: Duration) -> String {
    format!(
        "[{input} in / {output} out, {elapsed}]",
        input = format_count(usage.input_tokens),
        output = format_count(usage.output_tokens),
        elapsed = format_elapsed(elapsed),
    )
}

/// Render an elapsed duration as `{secs}.{tenths}s` or
/// `{mins}m {secs}.{tenths}s`.
fn format_elapsed(elapsed: Duration) -> String {
    let total_millis = elapsed.as_millis();
    let total_secs = total_millis / 1000;
    let tenths = (total_millis % 1000) / 100;
    if total_secs < 60 {
        format!("{total_secs}.{tenths}s")
    } else {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}m {secs}.{tenths}s")
    }
}

/// Extract the `tool_use_description` envelope field from a raw
/// arguments JSON string.
///
/// Returns `None` when the arguments don't parse, when the envelope
/// field is absent, or when the field is present but empty/whitespace.
pub(crate) fn extract_tool_use_description(arguments: &str) -> Option<String> {
    let raw: Value = serde_json::from_str(arguments).ok()?;
    let split = split_envelope_fields(raw);
    split
        .description
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
}

/// Flush the session store's persistence sink: pending durability work
/// and the index-registered sink's accumulated delta (event counts,
/// usage totals, `updated_at`) land now instead of at drop, so the
/// session's `index.jsonl` entry stays current across turns and an
/// abort cannot lose the delta. A no-op for sink-less stores
/// (ephemeral / `--no-session` mode).
///
/// Mirrors the print orchestrator's post-turn / post-`/compact`
/// checkpoint (`norn-cli::print::orchestrator::checkpoint_session`),
/// adapted to the TUI's error surface: a checkpoint failure must never
/// abort the turn — the conversation on screen is intact and the
/// JSONL event file is write-through — so the failure is logged via
/// `tracing::warn!` and returned as a message for the caller to write
/// in the red error-line style.
pub(crate) fn checkpoint_session(store: &norn::session::store::EventStore) -> Option<String> {
    match store.checkpoint() {
        Ok(()) => None,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "session checkpoint failed; the session index entry will lag \
                 until the next successful checkpoint or clean shutdown",
            );
            Some(format!("session checkpoint failed: {err}"))
        }
    }
}

/// Extract a short argument summary from the tool's inner arguments.
///
/// Falls back to common field names (`file_path`, `command`, `pattern`,
/// `query`, `path`) and returns the first non-empty string value found.
/// Used by the activity log as a fallback when the model omits
/// `tool_use_description`.
pub(crate) fn extract_argument_summary(arguments: &str) -> Option<String> {
    let raw: Value = serde_json::from_str(arguments).ok()?;
    let split = split_envelope_fields(raw);
    let obj = split.tool_args.as_object()?;
    for key in ["file_path", "command", "pattern", "query", "path"] {
        if let Some(val) = obj.get(key).and_then(Value::as_str) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    use norn::session::events::{EventBase, SessionEvent};
    use norn::session::store::EventStore;
    use norn::session::{CreateSessionOptions, DurabilityPolicy, SessionManager, read_index};

    fn session_with_registered_sink(data_dir: &std::path::Path) -> (String, EventStore) {
        let opened = SessionManager::new(data_dir)
            .create(
                CreateSessionOptions {
                    model: "test-model".to_owned(),
                    working_dir: "/tmp/work".to_owned(),
                    name: None,
                },
                DurabilityPolicy::Flush,
            )
            .unwrap();
        (opened.entry.id, opened.store)
    }

    /// Turn-boundary regression for the stale-index seam: under
    /// `DurabilityPolicy::Flush` the index delta is batched in the sink,
    /// so without an explicit checkpoint the entry only updates at drop
    /// (clean shutdown) — an abort loses it. `checkpoint_session` must
    /// land the delta while the store stays live across turns.
    #[test]
    fn checkpoint_session_flushes_index_delta_without_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let (id, store) = session_with_registered_sink(tmp.path());
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "turn one".to_owned(),
            })
            .unwrap();

        let before = read_index(tmp.path()).unwrap();
        let entry = before.iter().find(|e| e.id == id).unwrap();
        assert_eq!(
            entry.event_count, 0,
            "precondition: Flush policy batches the index delta until checkpoint",
        );

        assert_eq!(
            checkpoint_session(&store),
            None,
            "successful checkpoint reports no error",
        );

        let after = read_index(tmp.path()).unwrap();
        let entry = after.iter().find(|e| e.id == id).unwrap();
        assert_eq!(
            entry.event_count, 1,
            "checkpoint must flush the pending index delta while the store lives",
        );
        // Keep the store alive past the assertion so a drop-flush cannot
        // mask a checkpoint that did nothing.
        drop(store);
    }

    /// A sink-less store (`--no-session`) checkpoints as a no-op.
    #[test]
    fn checkpoint_session_sinkless_store_is_noop() {
        let store = EventStore::new();
        assert_eq!(checkpoint_session(&store), None);
    }

    /// Checkpoint failure surfaces a message (for the red error-line
    /// style) instead of panicking or aborting — the turn must survive.
    #[test]
    fn checkpoint_session_failure_returns_message() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&data_dir).unwrap();
        let (_id, store) = session_with_registered_sink(&data_dir);
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "doomed".to_owned(),
            })
            .unwrap();

        // Destroy the data directory so the index rewrite cannot land.
        std::fs::remove_dir_all(&data_dir).unwrap();

        let message = checkpoint_session(&store)
            .expect("checkpoint against a destroyed data dir must surface a failure");
        assert!(
            message.contains("session checkpoint failed"),
            "message must identify the failure: {message}",
        );
    }

    #[test]
    fn dim_line_count_single_line() {
        assert_eq!(dim_line_count("hello", 80), 1);
    }

    #[test]
    fn dim_line_count_wrapping() {
        let text = "a".repeat(160);
        assert_eq!(dim_line_count(&text, 80), 2);
    }

    #[test]
    fn dim_line_count_strips_ansi() {
        assert_eq!(dim_line_count("\x1b[2mhello\x1b[22m", 80), 1);
    }

    #[test]
    fn dim_line_count_empty() {
        assert_eq!(dim_line_count("", 80), 0);
    }

    #[test]
    fn visible_width_strips_csi() {
        assert_eq!(visible_width("\x1b[2mhi\x1b[22m"), 2);
    }

    #[test]
    fn visible_width_plain_text() {
        assert_eq!(visible_width("hello"), 5);
    }

    #[test]
    fn format_usage_summary_shape() {
        let usage = Usage {
            input_tokens: 1_234,
            output_tokens: 5_678,
            ..Usage::default()
        };
        let summary = format_usage_summary(&usage, Duration::from_millis(1_200));
        assert!(summary.contains("1,234 in"));
        assert!(summary.contains("5,678 out"));
        assert!(summary.contains("1.2s"));
    }

    #[test]
    fn format_elapsed_minutes() {
        let s = format_elapsed(Duration::from_secs(125));
        assert_eq!(s, "2m 5.0s");
    }

    #[test]
    fn extract_tool_use_description_from_envelope() {
        let args = r#"{"tool_use_description": "reading config", "file_path": "/etc/hosts"}"#;
        assert_eq!(
            extract_tool_use_description(args).as_deref(),
            Some("reading config"),
        );
    }

    #[test]
    fn extract_tool_use_description_empty_is_none() {
        let args = r#"{"tool_use_description": "  ", "command": "ls"}"#;
        assert!(extract_tool_use_description(args).is_none());
    }

    #[test]
    fn extract_argument_summary_file_path() {
        let args = r#"{"file_path": "/Users/tom/DESIGN.md"}"#;
        assert_eq!(
            extract_argument_summary(args).as_deref(),
            Some("/Users/tom/DESIGN.md"),
        );
    }

    #[test]
    fn extract_argument_summary_command() {
        let args = r#"{"command": "cargo test"}"#;
        assert_eq!(
            extract_argument_summary(args).as_deref(),
            Some("cargo test"),
        );
    }

    #[test]
    fn extract_argument_summary_prefers_file_path_over_command() {
        let args = r#"{"file_path": "/foo.rs", "command": "cat /foo.rs"}"#;
        assert_eq!(extract_argument_summary(args).as_deref(), Some("/foo.rs"),);
    }

    #[test]
    fn extract_argument_summary_skips_envelope() {
        let args = r#"{"tool_use_description": "reading", "pattern": "TODO"}"#;
        assert_eq!(extract_argument_summary(args).as_deref(), Some("TODO"),);
    }

    #[test]
    fn extract_argument_summary_returns_none_for_empty() {
        let args = r#"{"other_field": 42}"#;
        assert!(extract_argument_summary(args).is_none());
    }
}
