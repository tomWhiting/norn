//! Launch-time wiring for [`super::spawn::SpawnAgentTool`].
//!
//! This module creates the child channels, installs the child-local runtime
//! services, and returns the parent-held handle. The long-lived step/park/wake
//! lifecycle is owned by [`super::spawn_controller`].

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use parking_lot::RwLock;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use super::handle::{AgentHandle, AgentWakeRegistry, ChildBranchMetadata};
use super::infra::SubAgentExecutor;
use super::lifecycle::LifecycleEmitter;
use super::reclaim::ReclaimHandshake;
#[cfg(test)]
#[cfg(test)]
pub(crate) use super::spawn_completion::requeue_stranded_inbound;
use super::spawn_controller::SpawnController;
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::agent::result_channel::ChildResultSender;
use crate::agent::{PendingAgentMessages, PendingMailboxLease};
use crate::integration::hooks::HookRegistry;
use crate::r#loop::config::{AgentLoopConfig, ToolExecutor};
use crate::r#loop::inbound::inbound_channel;
use crate::r#loop::loop_context::LoopContext;
use crate::provider::agent_event::AgentEventSender;
use crate::provider::request::ToolDefinition;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;

/// Resources moved into a spawned child's controller task.
pub(super) struct ChildLaunch {
    pub(super) provider: Arc<dyn Provider>,
    pub(super) executor: SubAgentExecutor,
    pub(super) store: Arc<EventStore>,
    pub(super) loop_ctx: LoopContext,
    pub(super) tool_defs: Vec<ToolDefinition>,
    pub(super) task: String,
    pub(super) output_schema: Option<serde_json::Value>,
    pub(super) model: String,
    pub(super) agent_registry: Arc<RwLock<AgentRegistry>>,
    pub(super) result_sender: Option<ChildResultSender>,
    pub(super) child_id: Uuid,
    pub(super) branch_metadata: ChildBranchMetadata,
    pub(super) hooks: Option<Arc<HookRegistry>>,
    /// Profile/role label supplied to the sub-agent stop hook.
    pub(super) role_label: String,
    pub(super) event_sender: Option<AgentEventSender>,
    pub(super) reclaim: Option<ReclaimHandshake>,
    pub(super) lifecycle: LifecycleEmitter,
    pub(super) router: Arc<MessageRouter>,
    pub(super) inbound_capacity: usize,
    /// Fully resolved and validated child loop configuration.
    pub(super) config: AgentLoopConfig,
    pub(super) cancel: tokio_util::sync::CancellationToken,
    pub(super) wake_registry: Option<Arc<AgentWakeRegistry>>,
    /// Natural completion parks persistent children for a later wake.
    pub(super) persistent: bool,
    pub(super) pending_messages: Arc<PendingAgentMessages>,
    /// Controller-owned proof that this mailbox remains live.
    pub(super) mailbox_lease: Arc<PendingMailboxLease>,
}

/// Launch the child controller and return the handle retained by its parent.
pub(super) fn launch_child(launch: ChildLaunch) -> AgentHandle {
    let ChildLaunch {
        provider,
        executor,
        store,
        mut loop_ctx,
        tool_defs,
        task,
        output_schema,
        model,
        agent_registry,
        result_sender,
        child_id,
        branch_metadata,
        hooks,
        role_label,
        event_sender,
        reclaim,
        lifecycle,
        router,
        inbound_capacity,
        config,
        cancel,
        wake_registry,
        persistent,
        pending_messages,
        mailbox_lease,
    } = launch;

    let handle_store = Arc::clone(&store);
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    let (inbound_tx, inbound_rx) = inbound_channel(inbound_capacity);
    let (wake_tx, wake_rx) = mpsc::channel::<()>(1);
    let wake_pending = Arc::new(AtomicBool::new(false));

    // The handle is not observable until this function returns, so its first
    // Active state is published with a live route already installed.
    router.register(child_id, inbound_tx.clone());

    // Child-local schedules and processes are owned by the controller's loop
    // context and therefore cannot outlive the spawned child.
    if let Some(child_ctx) = executor.shared_context() {
        loop_ctx.schedule_executor = Some(crate::schedule::arm_schedule_executor(
            child_ctx.as_ref(),
            Arc::new(crate::schedule::ScheduleStore::new()),
            crate::schedule::ScheduleDelivery {
                agent_id: child_id,
                inbound: Some(inbound_tx.clone()),
                pending: loop_ctx.pending_agent_messages.clone(),
                event_store: Arc::clone(&store),
                registry: Some(Arc::clone(&agent_registry)),
                wake_registry: child_ctx.get_extension::<AgentWakeRegistry>(),
            },
        ));
        crate::agent::arming::arm_process_manager(
            child_ctx.as_ref(),
            &mut loop_ctx,
            &store,
            child_id,
            Some(inbound_tx.clone()),
            Some(Arc::clone(&agent_registry)),
        );
    } else {
        tracing::error!(
            child_id = %child_id,
            "spawn launch: the child executor exposes no shared tool context; \
             the schedule executor cannot arm and the cron tool will not resolve",
        );
    }

    let controller = SpawnController {
        provider,
        executor,
        store,
        loop_ctx,
        tool_defs,
        task,
        output_schema,
        model,
        agent_registry,
        result_sender,
        child_id,
        hooks,
        role_label,
        event_sender,
        reclaim,
        lifecycle,
        router,
        child_config: config,
        run_cancel: cancel.clone(),
        wake_registry,
        persistent,
        pending_messages,
        mailbox_lease,
        status_tx,
        inbound_tx: inbound_tx.clone(),
        inbound_rx,
        wake_rx,
        wake_pending: Arc::clone(&wake_pending),
    };
    let join_handle = tokio::spawn(controller.run());

    AgentHandle {
        agent_id: child_id,
        status_rx,
        inbound_tx,
        wake_tx,
        wake_pending,
        cancel,
        join_handle,
        event_store: handle_store,
        branch_metadata,
    }
}
