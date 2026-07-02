//! The Norn side of the driven-mode `intervene/*` control channel.
//!
//! This is the single adapter point (`DRIVEN-PROTOCOL.md` "Interventions")
//! where the neutral intervention primitives the JSON-RPC transport parses
//! ([`super::jsonrpc`]) are mapped onto Norn's native control channel:
//!
//! - `intervene/injectMessage` → a [`ChannelMessage`] delivered to the root
//!   agent through the harness [`MessageRouter`] — `Interrupt` priority as a
//!   [`MessageKind::Steer`] (drains at the next tool boundary), `Normal` as
//!   a [`MessageKind::Update`] (batches to stop-time). The injection is
//!   attributed to the OPERATOR, never a peer agent: `sender_id` is the nil
//!   UUID and `from` is the literal `operator`, both escaped by the harness
//!   frame builder, so a forged-identity injection can never impersonate an
//!   agent (the frame-message security contract).
//! - `intervene/cancel` → a trip of the run's [`CancellationToken`], the
//!   same token threaded into `AgentStepRequest.cancel`; the run loop
//!   observes it at the next boundary and returns
//!   `AgentStepResult::Cancelled`.
//!
//! Nothing above this file (the transport, the wire, a future server) names
//! a Norn type: the [`super::jsonrpc`] transport calls the neutral
//! [`InterventionHandler`] trait
//! and this struct is the Norn implementation of it.

use std::sync::Arc;

use chrono::Utc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use norn::agent::message_router::MessageRouter;
use norn::agent_loop::inbound::{ChannelMessage, MessageKind};

use super::jsonrpc::{InjectPriority, InterventionHandler};

/// The Norn control-channel adapter for driven-mode interventions.
///
/// Holds exactly the two handles the two advertised primitives need: the
/// harness [`MessageRouter`] plus the root agent id (for injection) and the
/// run's [`CancellationToken`] (for cancel). Cloneable and cheap — every
/// field is an `Arc`/`Copy` handle — so the concurrent intervene reader can
/// own one while the run owns the token clone it was built from.
#[derive(Clone)]
pub struct NornInterventionHandler {
    /// The harness router; `try_deliver` enqueues onto the root's bounded
    /// inbound channel without awaiting.
    router: Arc<MessageRouter>,
    /// The running root agent — the injection recipient.
    root_id: Uuid,
    /// The run's cancellation token; tripping it stops the step.
    cancel: CancellationToken,
}

impl NornInterventionHandler {
    /// Build the adapter over the run's router, root agent id, and cancel
    /// token.
    #[must_use]
    pub fn new(router: Arc<MessageRouter>, root_id: Uuid, cancel: CancellationToken) -> Self {
        Self {
            router,
            root_id,
            cancel,
        }
    }

    /// The operator-attributed [`ChannelMessage`] for an injected turn.
    ///
    /// Attribution is fixed operator ground truth — the nil sender id and
    /// the literal `operator` label — NOT any peer-agent identity, and the
    /// harness frame builder escapes both, so the injection can never forge
    /// a peer frame. `to_id`/`seq` are stamped by the router on delivery.
    fn operator_message(&self, text: &str, kind: MessageKind) -> ChannelMessage {
        ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: Uuid::nil(),
            from: "operator".to_owned(),
            role: None,
            to_id: self.root_id,
            content: text.to_owned(),
            kind,
            seq: None,
            timestamp: Utc::now(),
        }
    }
}

impl InterventionHandler for NornInterventionHandler {
    fn inject_message(&self, text: &str, priority: InjectPriority) -> Result<(), String> {
        let kind = match priority {
            InjectPriority::Interrupt => MessageKind::Steer,
            InjectPriority::Normal => MessageKind::Update,
        };
        let msg = self.operator_message(text, kind);
        self.router
            .try_deliver(self.root_id, msg)
            .map(|_seq| ())
            .map_err(|err| err.to_string())
    }

    fn cancel(&self, _reason: &str) -> Result<(), String> {
        // Tripping the shared token is infallible and idempotent: the run
        // loop checks it at the next boundary and returns Cancelled. The
        // reason rode the ack already; it is not re-encoded into the run.
        self.cancel.cancel();
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use norn::agent_loop::inbound::{InboundChannel, inbound_channel};

    /// Register a live root route and return the handler plus the receiving
    /// inbound channel, so a test can assert the injected message lands.
    fn wired_handler() -> (
        NornInterventionHandler,
        Uuid,
        InboundChannel,
        CancellationToken,
    ) {
        let router = Arc::new(MessageRouter::new());
        let root_id = Uuid::new_v4();
        let (tx, rx) = inbound_channel(8);
        router.register(root_id, tx);
        let token = CancellationToken::new();
        let handler = NornInterventionHandler::new(Arc::clone(&router), root_id, token.clone());
        (handler, root_id, rx, token)
    }

    #[test]
    fn interrupt_injection_delivers_a_steer_attributed_to_operator() {
        let (handler, _root, mut rx, _token) = wired_handler();
        handler
            .inject_message("stop and reconsider", InjectPriority::Interrupt)
            .expect("interrupt injection delivers");
        let drained = rx.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].kind, MessageKind::Steer);
        assert_eq!(drained[0].content, "stop and reconsider");
        // Operator attribution, never a peer agent.
        assert_eq!(drained[0].from, "operator");
        assert_eq!(drained[0].sender_id, Uuid::nil());
    }

    #[test]
    fn normal_injection_delivers_a_queued_update() {
        let (handler, _root, mut rx, _token) = wired_handler();
        handler
            .inject_message("fyi context", InjectPriority::Normal)
            .expect("normal injection delivers");
        let drained = rx.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].kind, MessageKind::Update);
        assert_eq!(drained[0].content, "fyi context");
    }

    #[test]
    fn cancel_trips_the_shared_token() {
        let (handler, _root, _rx, token) = wired_handler();
        assert!(!token.is_cancelled());
        handler
            .cancel("operator asked to stop")
            .expect("cancel is infallible");
        assert!(token.is_cancelled(), "the run's token is tripped");
    }

    #[test]
    fn injection_after_agent_channel_dropped_surfaces_the_error() {
        let (handler, _root, rx, _token) = wired_handler();
        drop(rx);
        // A closed recipient channel is surfaced as an error reason, never
        // swallowed — it rides the intervene/* error response.
        let err = handler
            .inject_message("into the void", InjectPriority::Interrupt)
            .expect_err("delivery into a dropped channel must fail");
        assert!(!err.is_empty());
    }
}
