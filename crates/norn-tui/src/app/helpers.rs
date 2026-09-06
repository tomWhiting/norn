//! Shared usage/argument helpers, terminal frame publication and session checkpointing.

use crate::TuiError;
use crate::render::text::format_count;
use crate::terminal::caps::TerminalCaps;
use crate::terminal::setup::TerminalGuard;
use norn::provider::usage::Usage;
use norn::tool::split_envelope_fields;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Publish one prepared frame; always finish synchronized output after an I/O failure.
pub(crate) fn sync_with_guard(
    caps: &TerminalCaps,
    guard: &mut TerminalGuard,
    previous: &mut Option<crate::render::frame::PreparedFrame>,
    prepared: crate::render::frame::PreparedFrame,
) -> Result<(), TuiError> {
    prepared.publish(previous, guard.terminal_mut(), caps.synchronized_rendering)?;
    Ok(())
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
///
/// Awaits [`norn::session::store::EventStore::checkpoint_off_executor`]:
/// the blocking critical section (inter-process index lock + full index
/// rewrite + fsync) runs on Tokio's blocking pool, never on the
/// executor thread driving the TUI event loop.
pub(crate) async fn checkpoint_session(
    store: &Arc<norn::session::store::EventStore>,
) -> Option<String> {
    match Arc::clone(store).checkpoint_off_executor().await {
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
mod tests {
    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    use super::*;

    use norn::session::events::{EventBase, SessionEvent};
    use norn::session::store::EventStore;
    use norn::session::{CreateSessionOptions, DurabilityPolicy, SessionManager, read_index};

    fn session_with_registered_sink(
        data_dir: &std::path::Path,
    ) -> TestResult<(String, EventStore)> {
        let opened = SessionManager::new(data_dir).create(
            CreateSessionOptions {
                model: "test-model".to_owned(),
                working_dir: "/tmp/work".to_owned(),
                name: None,
            },
            DurabilityPolicy::Flush,
        )?;
        Ok((opened.entry.id, opened.store))
    }

    /// Turn-boundary regression for the stale-index seam: under
    /// `DurabilityPolicy::Flush` the index delta is batched in the sink,
    /// so without an explicit checkpoint the entry only updates at drop
    /// (clean shutdown) — an abort loses it. `checkpoint_session` must
    /// land the delta while the store stays live across turns.
    #[tokio::test]
    async fn checkpoint_session_flushes_index_delta_without_drop() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let (id, store) = session_with_registered_sink(tmp.path())?;
        let store = Arc::new(store);
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "turn one".to_owned(),
        })?;

        let before = read_index(tmp.path())?;
        let entry = before
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("session {id} is missing before checkpoint"))?;
        assert_eq!(
            entry.event_count, 0,
            "precondition: Flush policy batches the index delta until checkpoint",
        );

        assert_eq!(
            checkpoint_session(&store).await,
            None,
            "successful checkpoint reports no error",
        );

        let after = read_index(tmp.path())?;
        let entry = after
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("session {id} is missing after checkpoint"))?;
        assert_eq!(
            entry.event_count, 1,
            "checkpoint must flush the pending index delta while the store lives",
        );
        // Keep the store alive past the assertion so a drop-flush cannot
        // mask a checkpoint that did nothing.
        drop(store);
        Ok(())
    }

    /// A sink-less store (`--no-session`) checkpoints as a no-op.
    #[tokio::test]
    async fn checkpoint_session_sinkless_store_is_noop() {
        let store = Arc::new(EventStore::new());
        assert_eq!(checkpoint_session(&store).await, None);
    }

    /// Checkpoint failure surfaces a message (for the red error-line
    /// style) instead of panicking or aborting — the turn must survive.
    #[tokio::test]
    async fn checkpoint_session_failure_returns_message() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let data_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&data_dir)?;
        let (_, store) = session_with_registered_sink(&data_dir)?;
        let store = Arc::new(store);
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "doomed".to_owned(),
        })?;

        // Destroy the data directory so the index rewrite cannot land.
        std::fs::remove_dir_all(&data_dir)?;

        let message = checkpoint_session(&store)
            .await
            .ok_or("checkpoint against a destroyed data dir must surface a failure")?;
        assert!(
            message.contains("session checkpoint failed"),
            "message must identify the failure: {message}",
        );
        Ok(())
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
