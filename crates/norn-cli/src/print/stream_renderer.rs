//! The background stream renderer for `-f stream-json` (NC-003 R8).
//!
//! Consumes [`norn::provider::AgentEvent`]s from the broadcast channel
//! and writes one NDJSON object per line to stdout as they arrive; the
//! per-event payload shape lives in [`super::output`]
//! (`agent_event_to_ndjson`), which is also the single source of truth
//! for the driven `event/*` notification payloads. Split out of
//! `output.rs` so that module stays within the 500-line budget.

use std::io::Write;

use tokio::sync::broadcast::error::RecvError;

use super::output::agent_event_to_ndjson;

/// Handle to the background stream renderer spawned by
/// [`spawn_stream_renderer`].
///
/// The renderer cannot rely on broadcast-channel closure to terminate:
/// the tool registry's shared `ToolContext` holds a
/// [`norn::provider::SharedAgentEventChannel`] extension with an owned
/// `Sender` clone (for subagent event forwarding), so the channel stays
/// open for as long as the runtime exists — awaiting closure alone hangs
/// forever (REVIEW C1). [`Self::finish`] sends an explicit shutdown
/// signal; the renderer drains every event already buffered on its
/// receiver, writes them, and exits.
pub struct StreamRendererHandle {
    /// Explicit shutdown trigger consumed by [`Self::finish`].
    shutdown: tokio::sync::oneshot::Sender<()>,
    /// The renderer task itself.
    task: tokio::task::JoinHandle<()>,
}

impl StreamRendererHandle {
    /// Signal the renderer to drain its buffered events and stop, then
    /// wait for the task to finish.
    ///
    /// Call this only after the step's own senders have been dropped —
    /// events broadcast after the shutdown signal are not rendered.
    ///
    /// # Errors
    ///
    /// Returns the [`tokio::task::JoinError`] when the renderer task
    /// panicked or was cancelled.
    pub async fn finish(self) -> Result<(), tokio::task::JoinError> {
        // A failed send means the receiver half is gone, i.e. the task
        // already exited on its own (channel closed / stdout broken) —
        // not an error; the join below still completes.
        let _ = self.shutdown.send(());
        self.task.await
    }
}

/// Spawn the streaming renderer for `stream-json` mode (NC-003 R8).
///
/// Subscribes to `tx`, then writes one NDJSON object per line to stdout
/// for every [`norn::provider::events::ProviderEvent`]. The task exits
/// when the broadcast sender is dropped, when the
/// [`StreamRendererHandle::finish`] shutdown signal fires (after
/// draining the events already buffered), or when stdout breaks. Lagged
/// receivers skip the missed events (best-effort — downstream pipes may
/// miss events; the brief accepts this trade-off).
///
/// When `partial` is `false` (the default), only complete events are
/// emitted: `text`, `thinking`, `tool_call`, `tool_result`, `done`.
/// Delta events (`text_delta`, `thinking_delta`, `tool_call_delta`) are
/// silently consumed. When `partial` is `true`, all events are emitted.
///
/// Returns a [`StreamRendererHandle`]; callers MUST terminate the task
/// via [`StreamRendererHandle::finish`] rather than awaiting channel
/// closure — the registry's shared-context sender clone keeps the
/// channel open for the lifetime of the runtime (REVIEW C1).
#[must_use]
pub fn spawn_stream_renderer(
    tx: &tokio::sync::broadcast::Sender<norn::provider::AgentEvent>,
    partial: bool,
) -> StreamRendererHandle {
    let mut rx = tx.subscribe();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // Biased: always drain events that are already ready
                // before observing shutdown, so nothing broadcast ahead
                // of the signal is dropped. The shutdown branch returns
                // immediately, so the completed oneshot is never polled
                // again.
                biased;
                received = rx.recv() => match received {
                    Ok(agent_event) => {
                        if !write_stream_event(&agent_event, partial) {
                            return;
                        }
                    }
                    Err(RecvError::Closed) => return,
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "stream renderer lagged — {n} events dropped");
                    }
                },
                // Resolves on the explicit signal AND when the handle is
                // dropped without calling finish() — the renderer must
                // never outlive its orchestrator.
                _ = &mut shutdown_rx => {
                    drain_buffered_events(&mut rx, partial);
                    return;
                }
            }
        }
    });
    StreamRendererHandle {
        shutdown: shutdown_tx,
        task,
    }
}

/// Write one agent event as an NDJSON line on stdout, honouring the
/// `partial` delta filter. Returns `false` when stdout is gone (broken
/// pipe) and the renderer should stop.
fn write_stream_event(agent_event: &norn::provider::AgentEvent, partial: bool) -> bool {
    let Some(line) = agent_event_to_ndjson(agent_event, partial) else {
        return true;
    };
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(line.as_bytes()).is_ok()
        && stdout.write_all(b"\n").is_ok()
        && stdout.flush().is_ok()
}

/// Drain and render the events already buffered on `rx` after a
/// shutdown signal. `try_recv` never blocks, so this terminates even
/// while the shared-context sender clone keeps the channel open.
fn drain_buffered_events(
    rx: &mut tokio::sync::broadcast::Receiver<norn::provider::AgentEvent>,
    partial: bool,
) {
    use tokio::sync::broadcast::error::TryRecvError;
    loop {
        match rx.try_recv() {
            Ok(agent_event) => {
                if !write_stream_event(&agent_event, partial) {
                    return;
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Closed) => return,
            Err(TryRecvError::Lagged(n)) => {
                tracing::warn!(missed = n, "stream renderer lagged — {n} events dropped");
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// REVIEW C1 regression: the renderer must terminate via the
    /// explicit shutdown handle even while an outstanding `Sender`
    /// clone (the registry's `SharedAgentEventChannel` extension in
    /// production) keeps the broadcast channel open. Pre-fix, the task
    /// awaited `RecvError::Closed` forever and this test timed out.
    #[tokio::test]
    async fn renderer_finishes_despite_outstanding_sender_clone() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<norn::provider::AgentEvent>(16);
        // Simulates the SharedAgentEventChannel extension: a clone that
        // outlives the step and is never dropped before the await.
        let registry_clone = tx.clone();

        let handle = spawn_stream_renderer(&tx, false);
        drop(tx);

        tokio::time::timeout(std::time::Duration::from_secs(10), handle.finish())
            .await
            .expect("renderer must exit via explicit shutdown despite a live sender clone")
            .expect("renderer task must not panic");

        drop(registry_clone);
    }

    /// A renderer task that panics must surface the `JoinError` from
    /// `finish()` (the orchestrator maps it onto the agent-error exit
    /// path) — never be swallowed as a clean completion.
    #[tokio::test]
    async fn finish_surfaces_renderer_panic_as_join_error() {
        let (shutdown, _shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async {
            panic!("simulated renderer panic");
        });
        let handle = StreamRendererHandle { shutdown, task };
        let err = handle
            .finish()
            .await
            .expect_err("finish() must report the panicked task");
        assert!(err.is_panic(), "JoinError must carry the panic: {err}");
    }

    /// The legacy termination path still works: with every sender
    /// dropped the channel closes and the task exits without any
    /// shutdown signal (`finish()` then joins an already-finished task).
    #[tokio::test]
    async fn renderer_exits_on_channel_closure_without_shutdown_signal() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<norn::provider::AgentEvent>(16);
        let handle = spawn_stream_renderer(&tx, false);
        drop(tx);

        // Give the task a moment to observe Closed, then join via
        // finish(); the send side of the shutdown signal failing is
        // tolerated by design.
        tokio::time::timeout(std::time::Duration::from_secs(10), handle.finish())
            .await
            .expect("renderer must exit when the channel closes")
            .expect("renderer task must not panic");
    }
}
