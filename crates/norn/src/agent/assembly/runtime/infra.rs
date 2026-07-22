//! Inputs for installing root spawn/fork coordination infrastructure.

use std::sync::Arc;

use parking_lot::RwLock;
use uuid::Uuid;

use crate::agent::child_policy::CoordinationEnvelope;
use crate::agent::registry::AgentRegistry;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;

/// Complete set of runtime-owned inputs consumed by
/// [`super::super::tooling::install_agent_infra`].
pub(crate) struct AgentInfraParts {
    /// Shared agent registry the coordination tools resolve against.
    pub(crate) registry: Arc<RwLock<AgentRegistry>>,
    /// Provider shared with spawned and forked children.
    pub(crate) provider: Arc<dyn Provider>,
    /// The parent agent's session event store.
    pub(crate) event_store: Arc<EventStore>,
    /// The root agent's session-branching identity: the allocation authority
    /// its spawn/fork/Rhai children mint routes through. It is either a
    /// persisted-session root or a deliberate ephemeral root.
    pub(crate) session: Arc<crate::session::SessionBinding>,
    /// The parent agent's id.
    pub(crate) id: Uuid,
    /// Root-controller liveness proof retained by the root loop context.
    pub(crate) mailbox_lease: Arc<crate::agent::PendingMailboxLease>,
    /// The validated child policy and child-result channel capacity. Both
    /// capacities are explicit and non-zero before installation.
    pub(crate) envelope: CoordinationEnvelope,
    /// The root agent's inbound sender, when one was configured.
    pub(crate) root_inbound: Option<crate::r#loop::inbound::InboundSender>,
    /// The root run-cancellation token inherited by child run tokens. It is
    /// the same token used by the root run and `AgentHandle::cancel`.
    pub(crate) cancel: tokio_util::sync::CancellationToken,
    /// Whether result delivery reclaims terminal child registry entries.
    /// Embedded/headless runtimes enable this; drivers with their own status
    /// observer, such as the TUI, disable it to own reclamation themselves.
    pub(crate) terminal_reclamation: bool,
}
