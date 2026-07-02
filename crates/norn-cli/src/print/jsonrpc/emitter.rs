//! The live `event/*` notification emitter (`DRIVEN-PROTOCOL.md` "Event
//! notifications").

use serde_json::json;

use norn::provider::AgentEvent;

use super::super::output::{agent_event_method, agent_event_to_value};
use super::frames::{JSONRPC_VERSION, JsonRpcNotification, TransportError};
use super::writer::OutboundWriter;

/// Spawn the live `event/*` notification emitter.
///
/// Subscribes a SECOND receiver off the run's existing broadcast channel —
/// the broadcast fan-out means this composes with the run (and, in other
/// modes, the stream renderer) without interference. Each [`AgentEvent`] is
/// mapped to its `event/*` method and its byte-identical stream-json
/// payload, with `agent_id` / `agent_role` added, and emitted through the
/// single serializing writer LIVE as it occurs.
///
/// The task terminates on the explicit `shutdown` signal (after draining
/// events already buffered on its receiver) — mirroring the stream
/// renderer's REVIEW C1 discipline, because the registry's shared-context
/// sender clone keeps the broadcast channel open for the runtime's life.
#[must_use]
pub fn spawn_event_emitter(
    tx: &tokio::sync::broadcast::Sender<AgentEvent>,
    writer: OutboundWriter,
) -> EventEmitterHandle {
    let mut rx = tx.subscribe();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            tokio::select! {
                // Biased: drain ready events before observing shutdown so
                // nothing broadcast ahead of the signal is dropped.
                biased;
                received = rx.recv() => match received {
                    Ok(event) => {
                        if emit_one(&writer, &event).is_err() {
                            return;
                        }
                    }
                    Err(RecvError::Closed) => return,
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "jsonrpc event emitter lagged — {n} events dropped");
                    }
                },
                _ = &mut shutdown_rx => {
                    drain_events(&mut rx, &writer);
                    return;
                }
            }
        }
    });
    EventEmitterHandle {
        shutdown: shutdown_tx,
        task,
    }
}

/// Handle to the spawned event emitter; call [`Self::finish`] to stop it.
pub struct EventEmitterHandle {
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl EventEmitterHandle {
    /// Signal the emitter to drain its buffered events and stop, then wait
    /// for the task. Call only after the run's own senders are dropped.
    ///
    /// # Errors
    ///
    /// Returns the [`tokio::task::JoinError`] if the emitter task panicked
    /// or was cancelled.
    pub async fn finish(self) -> Result<(), tokio::task::JoinError> {
        // A failed send means the task already exited (channel closed /
        // stdout broken) — not an error; the join still completes.
        let _ = self.shutdown.send(());
        self.task.await
    }
}

/// Map one [`AgentEvent`] to an `event/*` notification and emit it.
///
/// Events with no on-wire form (filtered deltas, provider `Error`) are
/// skipped. `partial` is `true`: the driven channel forwards deltas so the
/// consumer sees the full live stream — `--partial` is a render concern
/// that deliberately does not apply to the transport.
fn emit_one(writer: &OutboundWriter, event: &AgentEvent) -> Result<(), TransportError> {
    let Some(mut params) = agent_event_to_value(event, true) else {
        return Ok(());
    };
    if let Some(obj) = params.as_object_mut() {
        // The stream-json translator drops agent identity, which is fine
        // for single-agent stdout but makes multi-agent events
        // unattributable. Add it to every notification
        // (`DRIVEN-PROTOCOL.md` "Event notifications").
        obj.insert("agent_id".to_owned(), json!(event.agent_id.to_string()));
        obj.insert(
            "agent_role".to_owned(),
            json!(event.agent_role.as_ref().to_owned()),
        );
    }
    let note = JsonRpcNotification {
        jsonrpc: JSONRPC_VERSION,
        method: agent_event_method(event),
        params,
    };
    writer.send_notification(&note)
}

/// Drain events already buffered on `rx` after a shutdown signal.
/// `try_recv` never blocks, so this terminates even while the shared
/// sender clone keeps the channel open.
fn drain_events(rx: &mut tokio::sync::broadcast::Receiver<AgentEvent>, writer: &OutboundWriter) {
    use tokio::sync::broadcast::error::TryRecvError;
    loop {
        match rx.try_recv() {
            Ok(event) => {
                if emit_one(writer, &event).is_err() {
                    return;
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Closed) => return,
            Err(TryRecvError::Lagged(n)) => {
                tracing::warn!(
                    missed = n,
                    "jsonrpc event emitter lagged — {n} events dropped"
                );
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use norn::provider::events::{ProviderEvent, StopReason};
    use norn::provider::usage::Usage;
    use norn::provider::{AgentEvent, AgentEventKind};
    use serde_json::Value;
    use std::sync::Arc;
    use uuid::Uuid;

    fn provider_event(kind: ProviderEvent) -> AgentEvent {
        AgentEvent {
            agent_id: Uuid::nil(),
            agent_role: Arc::from("root"),
            event: AgentEventKind::Provider(kind),
        }
    }

    #[tokio::test]
    async fn event_emitter_streams_notifications_never_responses() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
        let (writer, mut out_rx) = OutboundWriter::test_channel();
        let handle = spawn_event_emitter(&tx, writer);

        // One of each mapped AgentEventKind arm.
        tx.send(provider_event(ProviderEvent::TextComplete {
            text: "hi".to_owned(),
        }))
        .unwrap();
        tx.send(provider_event(ProviderEvent::ToolCallComplete {
            call_id: "c1".to_owned(),
            name: "read".to_owned(),
            arguments: "{}".to_owned(),
            kind: norn::provider::request::ToolCallKind::Function,
        }))
        .unwrap();
        tx.send(provider_event(ProviderEvent::ToolResult {
            tool_call_id: "c1".to_owned(),
            tool_name: "read".to_owned(),
            output: serde_json::json!("ok"),
            duration_ms: 3,
        }))
        .unwrap();
        tx.send(provider_event(ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        }))
        .unwrap();

        drop(tx);
        handle.finish().await.unwrap();

        let mut methods = Vec::new();
        while let Ok(frame) = out_rx.try_recv() {
            let parsed: Value = serde_json::from_str(&frame).unwrap();
            // NO event/* notification is ever a Response: no id field.
            assert!(
                parsed.get("id").is_none(),
                "notification must not carry an id: {frame}"
            );
            assert_eq!(parsed["jsonrpc"], "2.0");
            // agent_id / agent_role are attached to every notification.
            assert!(parsed["params"]["agent_id"].is_string());
            assert!(parsed["params"]["agent_role"].is_string());
            methods.push(parsed["method"].as_str().unwrap().to_owned());
        }
        assert!(methods.contains(&"event/message".to_owned()));
        assert!(methods.contains(&"event/toolCall".to_owned()));
        assert!(methods.contains(&"event/toolResult".to_owned()));
        assert!(methods.contains(&"event/stop".to_owned()));
    }
}
