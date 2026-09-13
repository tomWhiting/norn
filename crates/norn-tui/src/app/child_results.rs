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
        batch.extend(ready_frontier(rx));
    }
    for result in &batch {
        let detail = format_child_result_detail(result)?;
        notices::child_result(state, result.agent_id, &result.agent_role, &detail)?;
        let origin = format_child_result_origin(result)?;
        state.transcript.notice(
            norn::session_view::ViewItemKind::Metadata,
            &format!(
                "Result run metadata · {} ({})",
                result.agent_role, result.agent_id
            ),
            Some(&origin),
        )?;
    }
    pending_child_prompts.push_back(format_child_result_batch(&batch));
    Ok(())
}

/// Consume only the captured queue frontier, leaving later arrivals to the event
/// loop so a producer cannot extend this synchronous batch past keyboard input.
fn ready_frontier<T>(
    receiver: &mut tokio::sync::mpsc::Receiver<T>,
) -> impl Iterator<Item = T> + '_ {
    let remaining = receiver.len();
    // Empty and disconnected both end this batch; buffered results remain readable
    // after sender disconnection, and the normal receiver owns future arrivals.
    std::iter::from_fn(move || receiver.try_recv().ok()).take(remaining)
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

/// Original run metadata is separately inspectable without expanding normal result text.
fn format_child_result_origin(result: &ChildAgentResult) -> Result<String, TuiError> {
    let mut detail = String::new();
    if let Some(origin) = &result.origin {
        write!(
            detail,
            "Run: {}\nTrigger: {}\nStarted: {}\nCompleted: {}\nStore generation: {}",
            origin.run_id,
            origin.trigger.as_str(),
            origin
                .started_at
                .with_timezone(&chrono_tz::Australia::Melbourne)
                .format("%Y-%m-%d %H:%M:%S%.3f %Z"),
            origin
                .completed_at
                .with_timezone(&chrono_tz::Australia::Melbourne)
                .format("%Y-%m-%d %H:%M:%S%.3f %Z"),
            origin.source.store_generation,
        )
        .map_err(std::io::Error::other)?;
        let session = match &origin.source.session {
            norn::session_view::SessionIdentity::Persisted(id) => format!("persisted {id}"),
            norn::session_view::SessionIdentity::Ephemeral(id) => format!("ephemeral {id}"),
        };
        write!(
            detail,
            "\nSession: {session}\nTimeline before run: {}\nTimeline at completion: {}",
            origin
                .start_after_event
                .as_ref()
                .map_or("empty", norn::session::events::EventId::as_str),
            origin
                .end_at_event
                .as_ref()
                .map_or("empty", norn::session::events::EventId::as_str),
        )
        .map_err(std::io::Error::other)?;
    } else {
        detail.push_str("Run provenance: unavailable");
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

    #[test]
    fn result_origin_metadata_stays_out_of_the_normal_conversation()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::app::conversation_view::ConversationView;
        use crate::render::frame::Frame;
        use crate::render::layout::{Layout, Rect};

        let mut state = AppState::new(
            crate::terminal::caps::TerminalCaps::baseline(),
            crate::input::history::InputHistory::in_memory(),
            norn::agent::registry::AgentRegistry::shared(),
            crate::app::state::test_view_source(Uuid::new_v4()),
            crate::render::fixed_panel::StatusBar::default(),
        );
        render_child_result_batch(
            &mut state,
            &mut None,
            &mut PendingChildPrompts::new(),
            result("worker", "actual result"),
        )?;
        let area = Rect {
            column: 0,
            row: 0,
            width: 120,
            height: 40,
        };
        for expanded in [false, true] {
            state.transcript.config.expanded_tools = expanded;
            let mut frame = Frame {
                layout: Layout::NoPaint,
                rows: Vec::new(),
                composer: None,
                cursor: None,
            };
            crate::app::render::transcript::paint(
                &mut ConversationView::root(&mut state)?,
                &mut frame,
                area,
                None,
            )?;
            let metadata_visible = frame
                .rows
                .iter()
                .any(|row| row.text.styled.text().contains("Result run metadata"));
            assert_eq!(metadata_visible, expanded);
            assert!(
                frame
                    .rows
                    .iter()
                    .any(|row| row.text.styled.text().contains("Child worker"))
            );
        }
        Ok(())
    }

    #[test]
    fn refilling_producer_cannot_extend_the_captured_batch()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        sender.try_send(1)?;
        sender.try_send(2)?;
        {
            let mut batch = ready_frontier(&mut receiver);
            assert_eq!(batch.next(), Some(1));
            sender.try_send(3)?;
            assert_eq!(batch.next(), Some(2));
            sender.try_send(4)?;
            assert_eq!(batch.next(), None);
        }
        assert_eq!(receiver.try_recv()?, 3);
        assert_eq!(receiver.try_recv()?, 4);
        Ok(())
    }

    #[test]
    fn empty_frontier_does_not_consume_later_arrival() -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        {
            let mut batch = ready_frontier(&mut receiver);
            sender.try_send(1)?;
            assert_eq!(batch.next(), None);
        }
        assert_eq!(receiver.try_recv()?, 1);
        Ok(())
    }

    #[test]
    fn disconnected_frontier_retains_every_buffered_result()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        sender.try_send(1)?;
        sender.try_send(2)?;
        drop(sender);
        assert_eq!(ready_frontier(&mut receiver).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        );
        Ok(())
    }

    #[test]
    fn original_completion_renders_in_melbourne_with_seasonal_offset()
    -> Result<(), Box<dyn std::error::Error>> {
        for (utc, local) in [
            ("2026-01-13T04:15:48Z", "2026-01-13 15:15:48.000 AEDT"),
            ("2026-07-13T04:15:48Z", "2026-07-13 14:15:48.000 AEST"),
        ] {
            let mut child = result("worker", "retained result");
            let at = chrono::DateTime::parse_from_rfc3339(utc)?.with_timezone(&chrono::Utc);
            let run_id = Uuid::new_v4();
            child.origin = Some(norn::agent::result_origin::ChildResultOrigin {
                run_id,
                source: crate::app::state::test_view_source(child.agent_id),
                trigger: norn::agent::result_origin::ChildRunTrigger::InitialTask,
                started_at: at,
                completed_at: at,
                start_after_event: None,
                end_at_event: None,
            });
            let detail = format_child_result_origin(&child)?;
            assert!(detail.contains(&format!("Completed: {local}")), "{detail}");
            assert!(detail.contains(&run_id.to_string()));
            assert!(!detail.contains("provenance: unavailable"));
            assert!(detail.contains("Timeline before run: empty"));
        }
        Ok(())
    }

    fn result(role: &str, body: &str) -> ChildAgentResult {
        ChildAgentResult {
            origin: None,
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
        assert_eq!(
            state.transcript.projection.items().len(),
            children.len() * 2
        );
        assert_eq!(
            state
                .transcript
                .projection
                .items()
                .filter(|row| matches!(row.kind, ViewItemKind::Metadata))
                .count(),
            children.len()
        );
        for (row, child) in state
            .transcript
            .projection
            .items()
            .filter(|row| matches!(row.kind, ViewItemKind::Child))
            .zip(children)
        {
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
