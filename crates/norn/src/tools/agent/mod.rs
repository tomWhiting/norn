//! `SpawnAgent`, `SignalAgent`, `CloseAgent`, Fork tools.
//!
//! All four tools wrap the agent infrastructure delivered in N-014.
//! Implementations live in named submodules; this file only declares them
//! and re-exports the public surface.

pub mod coord;
pub(crate) mod delegation;
pub(super) mod fork_context;
pub(super) mod fork_launch;
pub(super) mod fork_outcome;
mod fork_seed;
pub mod fork_tool;
pub mod handle;
pub mod infra;
pub(crate) mod lifecycle;
mod live_tools;
mod mcp_selection;
pub mod reclaim;
pub mod spawn;
pub(crate) mod spawn_completion;
pub(crate) mod spawn_context;
pub(crate) mod spawn_controller;
pub(crate) mod spawn_launch;
pub(crate) mod spawn_outcome;
mod spawn_schema;
pub(crate) mod variant_resolve;

#[cfg(test)]
mod canonical_lifecycle_test_support;
#[cfg(test)]
mod fork_seed_canonical_tests;

pub use self::coord::{
    CloseAgentTool, SIGNAL_AGENT_TOOL_NAME, SignalAgentTool, WAKE_AGENT_TOOL_NAME, WakeAgentTool,
};
pub(crate) use self::fork_outcome::ForkOutcome;
pub use self::fork_tool::{FORK_TOOL_NAME, ForkTool};
pub use self::handle::{AgentHandle, AgentHandles, AgentWakeRegistry, WakeRequestOutcome};
pub use self::infra::{AgentCancellation, AgentModel, AgentToolInfra};
pub(crate) use self::lifecycle::append_message_audit;
pub use self::reclaim::ReclaimOnResultDelivery;
pub use self::spawn::{SPAWN_TOOL_NAME, SpawnAgentTool};

/// Parent-visible failure suffix for terminal mailbox persistence faults.
/// Deliberately carries no message identity or content.
const TERMINAL_PERSISTENCE_FAILURE: &str =
    "terminal agent-message persistence failed; accepted work was not confirmed durable";

#[cfg(test)]
pub(crate) struct TestChildEventStore(pub(crate) std::sync::Arc<crate::session::store::EventStore>);

/// Test-only suspension point after outcome projection and before terminal
/// mailbox transition. This keeps failure-injection timing out of production.
#[cfg(test)]
pub(crate) struct TestTerminalTransitionGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
impl TestTerminalTransitionGate {
    pub(crate) fn new() -> Self {
        Self {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        }
    }

    pub(crate) async fn hold(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }

    pub(crate) async fn wait_until_entered(&self) {
        self.entered.notified().await;
    }

    pub(crate) fn release(&self) {
        self.release.notify_one();
    }
}

#[cfg(test)]
pub(crate) fn terminal_queue_failure_store(
    diagnostic: &'static str,
) -> std::sync::Arc<crate::session::store::EventStore> {
    struct RejectTerminalQueue {
        diagnostic: &'static str,
    }

    impl crate::session::store::PersistenceSink for RejectTerminalQueue {
        fn persist(
            &mut self,
            event: &crate::session::events::SessionEvent,
        ) -> Result<(), crate::session::persistence::SessionPersistError> {
            if matches!(
                event,
                crate::session::events::SessionEvent::Custom { event_type, .. }
                    if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE
            ) {
                return Err(crate::session::persistence::SessionPersistError::Io(
                    std::io::Error::other(format!(
                        "{}; rejected queue event: {event:?}",
                        self.diagnostic
                    )),
                ));
            }
            Ok(())
        }
    }

    std::sync::Arc::new(crate::session::store::EventStore::with_sink(Box::new(
        RejectTerminalQueue { diagnostic },
    )))
}

#[cfg(test)]
#[path = "spawn_mcp_tests.rs"]
mod spawn_mcp_tests;
