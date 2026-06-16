//! TUI delivery surface for completed child/fork agent results.
//!
//! The result channel is owned by the TUI event loop, not by the core
//! runner, so completed child results can be displayed immediately even
//! while the root turn is still streaming. The secure framed result is
//! queued as a follow-up root prompt and injected only at a safe turn
//! boundary.

use std::collections::VecDeque;

use norn::agent::result_channel::ChildAgentResult;

use crate::TuiError;
use crate::terminal::setup::TerminalGuard;

use super::render::write_user_message;
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

/// Drain all currently buffered child results and queue their framed
/// root prompts.
pub(super) fn drain_ready_child_results(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    child_rx: &mut ChildResultRx,
    pending_child_prompts: &mut PendingChildPrompts,
) -> Result<(), TuiError> {
    while let Some(result) = child_rx.as_mut().and_then(|rx| rx.try_recv().ok()) {
        render_child_result_batch(state, guard, child_rx, pending_child_prompts, result)?;
    }
    Ok(())
}

/// Render a visible completion summary and queue the corresponding
/// framed result for model delivery.
pub(super) fn render_child_result_batch(
    state: &mut AppState,
    guard: &mut TerminalGuard,
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
    let (display, prompt) = format_child_result_batch(&batch);
    write_user_message(&display, state, guard)?;
    pending_child_prompts.push_back(prompt);
    Ok(())
}

/// Build the display text and model prompt from a batch of completed
/// child/fork results.
pub(super) fn format_child_result_batch(batch: &[ChildAgentResult]) -> (String, String) {
    use norn::agent::result_channel::frame_child_result;

    if batch.len() == 1 {
        let result = &batch[0];
        let display = format!(
            "[{} completed]\n{}",
            result.agent_role, result.formatted_message,
        );
        return (display, frame_child_result(result));
    }

    let display = format!("[{} agents completed]", batch.len());
    let mut prompt = format!("Results from {} completed agents:\n\n", batch.len());
    for result in batch {
        prompt.push_str(&frame_child_result(result));
        prompt.push_str("\n\n");
    }
    (display, prompt)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
    fn single_result_has_visible_summary_and_framed_prompt() {
        let child = result("spawn/worker", "done");
        let id = child.agent_id;
        let (display, prompt) = format_child_result_batch(&[child]);

        assert_eq!(display, "[spawn/worker completed]\ndone");
        assert!(prompt.contains("<agent_result from=\"spawn/worker\""));
        assert!(prompt.contains(&format!("from_id=\"{id}\"")));
        assert!(prompt.contains("\ndone\n"));
    }

    #[test]
    fn multiple_results_batch_visible_summary_and_all_frames() {
        let batch = [
            result("spawn/a", "one"),
            result("fork/b", "two"),
            result("spawn/c", "three"),
        ];
        let (display, prompt) = format_child_result_batch(&batch);

        assert_eq!(display, "[3 agents completed]");
        assert_eq!(prompt.matches("<agent_result ").count(), 3);
        assert!(prompt.contains("Results from 3 completed agents"));
    }
}
