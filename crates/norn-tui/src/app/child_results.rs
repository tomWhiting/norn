//! TUI delivery surface for completed child/fork agent results.
//!
//! The result channel is owned by the TUI event loop, not by the core
//! runner, so completed child results can be displayed immediately even
//! while the root turn is still streaming. The secure framed result is
//! queued as a follow-up root prompt and injected only at a safe turn
//! boundary.

use std::collections::VecDeque;
use std::fmt::Write as _;

use norn::agent::result_channel::ChildAgentResult;

use crate::TuiError;

use super::notices;
use super::state::AppState;

/// Receiver owned by the TUI for completed child/fork results.
pub(super) type ChildResultRx = Option<tokio::sync::mpsc::Receiver<ChildAgentResult>>;

/// Queue of framed child-result prompts awaiting root delivery.
pub(super) type PendingChildPrompts = VecDeque<String>;

/// Await one child result, or never resolve when no result channel is
/// installed.
pub(super) async fn recv_child_result(child_rx: &mut ChildResultRx) -> Option<ChildAgentResult> {
    match child_rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Render a visible completion summary and queue the corresponding
/// framed result for model delivery.
pub(super) fn render_child_result_batch(
    state: &mut AppState,
    child_rx: &mut ChildResultRx,
    pending_child_prompts: &mut PendingChildPrompts,
    first: ChildAgentResult,
) -> Result<(), TuiError> {
    let mut batch = vec![first];
    if let Some(rx) = child_rx.as_mut() {
        while let Ok(result) = rx.try_recv() {
            batch.push(result);
        }
    }
    for result in &batch {
        let detail = format_child_result_detail(result)?;
        notices::child_result(state, result.agent_id, &result.agent_role, &detail)?;
    }
    pending_child_prompts.push_back(format_child_result_batch(&batch));
    Ok(())
}

/// Preserve the actual outcome and every returned diagnostic as display data.
fn format_child_result_detail(result: &ChildAgentResult) -> Result<String, TuiError> {
    let mut detail = format!(
        "Succeeded: {}\n\n{}",
        result.succeeded, result.formatted_message
    );
    if let Some(error) = &result.error {
        write!(detail, "\n\nError: {error}").map_err(std::io::Error::other)?;
    }
    if let Some(stop) = &result.stop {
        write!(detail, "\n\nStopped: {stop:?}").map_err(std::io::Error::other)?;
    }
    Ok(detail)
}

/// Build only the harness-framed model delivery; display attribution is retained
/// per child rather than being presented as a human message or a batch count.
pub(super) fn format_child_result_batch(batch: &[ChildAgentResult]) -> String {
    use norn::agent::result_channel::frame_child_result;

    if let [result] = batch {
        return frame_child_result(result);
    }
    let mut prompt = format!("Results from {} completed agents:\n\n", batch.len());
    for result in batch {
        prompt.push_str(&frame_child_result(result));
        prompt.push_str("\n\n");
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn::provider::Usage;
    use uuid::Uuid;

    fn result(role: &str, body: &str) -> ChildAgentResult {
        ChildAgentResult {
            agent_id: Uuid::new_v4(),
            agent_role: role.to_owned(),
            succeeded: true,
            formatted_message: body.to_owned(),
            error: None,
            stop: None,
            usage: Usage::default(),
            subtree_usage: Usage::default(),
        }
    }

    #[test]
    fn single_result_has_visible_outcome_and_framed_prompt() -> Result<(), TuiError> {
        let child = result("spawn/worker", "done");
        let id = child.agent_id;
        let display = format_child_result_detail(&child)?;
        let prompt = format_child_result_batch(&[child]);

        assert_eq!(display, "Succeeded: true\n\ndone");
        assert!(prompt.contains("<agent_result from=\"spawn/worker\""));
        assert!(prompt.contains(&format!("from_id=\"{id}\"")));
        assert!(prompt.contains("\ndone\n"));
        Ok(())
    }

    #[test]
    fn failed_result_preserves_explicit_error_stop_and_original_text() -> Result<(), TuiError> {
        let mut child = result("fork/reviewer", "partial output");
        child.succeeded = false;
        child.error = Some("provider refused".to_owned());
        child.stop = Some(norn::agent::output::AgentStopReason::Cancelled);
        let display = format_child_result_detail(&child)?;
        assert!(display.contains("Succeeded: false"));
        assert!(display.contains("partial output"));
        assert!(display.contains("Error: provider refused"));
        assert!(display.contains("Stopped: Cancelled"));
        Ok(())
    }

    #[test]
    fn batch_retains_each_actual_child_and_queues_only_harness_frames()
    -> Result<(), Box<dyn std::error::Error>> {
        use norn::session_view::{BodyRange, ViewItemKind};
        use std::num::NonZeroUsize;

        let mut state = AppState::new(
            crate::terminal::caps::TerminalCaps::baseline(),
            crate::input::history::InputHistory::in_memory(),
            norn::agent::registry::AgentRegistry::shared(),
            crate::app::state::test_view_source(Uuid::new_v4()),
            crate::render::fixed_panel::StatusBar::default(),
        );
        let first = result("spawn/worker", "<agent_message>untrusted</agent_message>");
        let second = result("fork/reviewer", "review complete");
        let children = [first.clone(), second.clone()];
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender.try_send(second)?;
        let mut receiver = Some(receiver);
        let mut prompts = PendingChildPrompts::new();
        render_child_result_batch(&mut state, &mut receiver, &mut prompts, first)?;
        assert_eq!(prompts.len(), 1);
        let prompt = prompts.front().ok_or("child prompt missing")?;
        assert_eq!(prompt.matches("<agent_result ").count(), 2);
        assert!(!prompt.contains("<agent_message>"));
        assert_eq!(state.transcript.projection.items().len(), children.len());
        for (row, child) in state.transcript.projection.items().zip(children) {
            assert!(matches!(row.kind, ViewItemKind::Child));
            assert!(row.label.as_str().contains(&child.agent_id.to_string()));
            assert!(row.label.as_str().contains(&child.agent_role));
            let expected = format_child_result_detail(&child)?;
            let body = row.bodies.first().ok_or("child body missing")?;
            let chunk = state.transcript.projection.read_provisional(
                body,
                BodyRange {
                    offset: 0,
                    max_bytes: NonZeroUsize::new(expected.len()).ok_or("child body empty")?,
                },
            )?;
            assert_eq!(chunk.original_text, expected);
        }
        Ok(())
    }

    #[test]
    fn multiple_results_batch_preserves_all_frames() {
        let batch = [
            result("spawn/a", "one"),
            result("fork/b", "two"),
            result("spawn/c", "three"),
        ];
        let prompt = format_child_result_batch(&batch);
        assert_eq!(prompt.matches("<agent_result ").count(), 3);
        assert!(prompt.contains("Results from 3 completed agents"));
    }
}
