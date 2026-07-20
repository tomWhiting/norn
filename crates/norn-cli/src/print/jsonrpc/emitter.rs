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
                // Once shutdown is ready, snapshot the unread prefix before
                // accepting more events. The registry intentionally keeps a
                // sender alive, and a faulty producer must not keep terminal
                // shutdown alive forever by continuously refilling `rx`.
                biased;
                _ = &mut shutdown_rx => {
                    let pending = rx.len();
                    return drain_events(&mut rx, &writer, pending);
                }
                received = rx.recv() => match received {
                    Ok(event) => emit_one(&writer, &event)?,
                    // Closing every producer is the normal end of the
                    // event stream. Unlike lag, it loses no accepted event.
                    Err(RecvError::Closed) => return Ok(()),
                    Err(RecvError::Lagged(missed)) => {
                        return Err(EventEmitterError::EventsLost { missed });
                    }
                },
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
    task: tokio::task::JoinHandle<Result<(), EventEmitterError>>,
}

/// A driven event stream ended before every accepted event was emitted.
#[derive(Debug, thiserror::Error)]
pub enum EventEmitterError {
    /// Serializing or enqueueing a notification failed.
    #[error("jsonrpc event emitter transport failed: {0}")]
    Transport(#[from] TransportError),
    /// The broadcast receiver fell behind and irretrievably lost events.
    #[error("jsonrpc event emitter lost {missed} events before delivery")]
    EventsLost {
        /// Number of events discarded by the broadcast channel.
        missed: u64,
    },
    /// The emitter task panicked or was cancelled.
    #[error("jsonrpc event emitter task failed: {0}")]
    Task(#[source] tokio::task::JoinError),
}

impl EventEmitterHandle {
    /// Signal the emitter to drain its buffered events and stop, then wait
    /// for the task. Call only after the run's own senders are dropped.
    ///
    /// # Errors
    ///
    /// Returns a typed failure if events were lost, a notification could
    /// not be enqueued, or the task panicked or was cancelled. A failed
    /// shutdown send is not itself an error: it means the task already
    /// completed, and its join result remains authoritative.
    pub async fn finish(self) -> Result<(), EventEmitterError> {
        let _ = self.shutdown.send(());
        match self.task.await {
            Ok(result) => result,
            Err(error) => Err(EventEmitterError::Task(error)),
        }
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

/// Drain the unread prefix observed when shutdown won the select.
///
/// The fixed count is load-bearing: draining until [`TryRecvError::Empty`]
/// would let a concurrently active producer extend shutdown without bound.
fn drain_events(
    rx: &mut tokio::sync::broadcast::Receiver<AgentEvent>,
    writer: &OutboundWriter,
    pending: usize,
) -> Result<(), EventEmitterError> {
    use tokio::sync::broadcast::error::TryRecvError;
    for _ in 0..pending {
        match rx.try_recv() {
            Ok(event) => emit_one(writer, &event)?,
            Err(TryRecvError::Empty | TryRecvError::Closed) => return Ok(()),
            Err(TryRecvError::Lagged(missed)) => {
                return Err(EventEmitterError::EventsLost { missed });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
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
    async fn event_emitter_streams_notifications_never_responses()
    -> Result<(), Box<dyn std::error::Error>> {
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
        let (writer, mut out_rx) = OutboundWriter::test_channel();
        let handle = spawn_event_emitter(&tx, writer);

        // One of each mapped AgentEventKind arm.
        assert!(
            tx.send(provider_event(ProviderEvent::TextComplete {
                text: "hi".to_owned(),
            }))
            .is_ok()
        );
        assert!(
            tx.send(provider_event(ProviderEvent::ToolCallComplete {
                call_id: "c1".to_owned(),
                name: "read".to_owned(),
                arguments: "{}".to_owned(),
                kind: norn::provider::request::ToolCallKind::Function,
            }))
            .is_ok()
        );
        assert!(
            tx.send(provider_event(ProviderEvent::ToolResult {
                tool_call_id: "c1".to_owned(),
                tool_name: "read".to_owned(),
                output: serde_json::json!("ok"),
                duration_ms: 3,
            }))
            .is_ok()
        );
        assert!(
            tx.send(provider_event(ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }))
            .is_ok()
        );

        drop(tx);
        handle.finish().await?;

        let mut methods = Vec::new();
        while let Ok(frame) = out_rx.try_recv() {
            let parsed: Value = serde_json::from_str(&frame)?;
            // NO event/* notification is ever a Response: no id field.
            assert!(
                parsed.get("id").is_none(),
                "notification must not carry an id: {frame}"
            );
            assert_eq!(parsed["jsonrpc"], "2.0");
            // agent_id / agent_role are attached to every notification.
            assert!(parsed["params"]["agent_id"].is_string());
            assert!(parsed["params"]["agent_role"].is_string());
            let method = parsed["method"]
                .as_str()
                .ok_or_else(|| std::io::Error::other("notification method was not a string"))?;
            methods.push(method.to_owned());
        }
        assert!(methods.contains(&"event/message".to_owned()));
        assert!(methods.contains(&"event/toolCall".to_owned()));
        assert!(methods.contains(&"event/toolResult".to_owned()));
        assert!(methods.contains(&"event/stop".to_owned()));
        Ok(())
    }

    #[tokio::test]
    async fn explicit_shutdown_drains_with_live_sender() -> Result<(), Box<dyn std::error::Error>> {
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(8);
        let (writer, mut out_rx) = OutboundWriter::test_channel();
        let handle = spawn_event_emitter(&tx, writer);
        assert!(
            tx.send(provider_event(ProviderEvent::TextComplete {
                text: "buffered before shutdown".to_owned(),
            }))
            .is_ok()
        );

        tokio::time::timeout(std::time::Duration::from_secs(10), handle.finish()).await??;
        let frame = out_rx.try_recv()?;
        let parsed: Value = serde_json::from_str(&frame)?;
        assert_eq!(parsed["method"], "event/message");
        assert_eq!(parsed["params"]["text"], "buffered before shutdown");
        drop(tx);
        Ok(())
    }

    #[test]
    fn shutdown_drain_is_bounded_by_its_unread_snapshot() -> Result<(), Box<dyn std::error::Error>>
    {
        let (tx, mut rx) = tokio::sync::broadcast::channel::<AgentEvent>(8);
        let (writer, mut out_rx) = OutboundWriter::test_channel();
        assert!(
            tx.send(provider_event(ProviderEvent::TextComplete {
                text: "before snapshot".to_owned(),
            }))
            .is_ok()
        );
        let pending = rx.len();
        assert_eq!(pending, 1);
        assert!(
            tx.send(provider_event(ProviderEvent::TextComplete {
                text: "after snapshot".to_owned(),
            }))
            .is_ok()
        );

        drain_events(&mut rx, &writer, pending)?;

        let frame: Value = serde_json::from_str(&out_rx.try_recv()?)?;
        assert_eq!(frame["params"]["text"], "before snapshot");
        assert!(
            out_rx.try_recv().is_err(),
            "post-snapshot event was drained"
        );
        assert_eq!(rx.len(), 1, "post-snapshot event must remain unread");
        Ok(())
    }

    #[tokio::test]
    async fn event_emitter_surfaces_outbound_receiver_loss() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(1);
        let (writer, out_rx) = OutboundWriter::test_channel();
        drop(out_rx);
        let handle = spawn_event_emitter(&tx, writer);

        let sent = tx.send(provider_event(ProviderEvent::TextComplete {
            text: "undeliverable".to_owned(),
        }));
        assert!(sent.is_ok());
        let outcome = handle.finish().await;

        assert!(
            matches!(
                outcome,
                Err(EventEmitterError::Transport(TransportError::WriterStopped(
                    _
                )))
            ),
            "outcome: {outcome:?}"
        );
    }

    #[tokio::test]
    async fn event_emitter_surfaces_broadcast_lag() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(1);
        let (writer, _out_rx) = OutboundWriter::test_channel();
        let handle = spawn_event_emitter(&tx, writer);

        for text in ["overwritten", "retained"] {
            let sent = tx.send(provider_event(ProviderEvent::TextComplete {
                text: text.to_owned(),
            }));
            assert!(sent.is_ok());
        }
        let outcome = handle.finish().await;

        assert!(
            matches!(outcome, Err(EventEmitterError::EventsLost { missed: 1 })),
            "outcome: {outcome:?}"
        );
    }
}
