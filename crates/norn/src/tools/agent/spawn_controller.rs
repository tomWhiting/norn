//! Long-lived controller loop for a spawned agent.
//!
//! Launch-time channel and tool-context wiring stays in
//! [`super::spawn_launch`]. This module owns the child step, route transition,
//! idle park, wake, and terminal mailbox-close sequence.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::FutureExt;
use parking_lot::RwLock;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use super::handle::AgentWakeRegistry;
use super::infra::SubAgentExecutor;
use super::lifecycle::{LifecycleEmitter, SubagentCompletion};
use super::reclaim::ReclaimHandshake;
use super::spawn_completion::{
    activate_route, deliver_step_result, mark_closed, mark_idle, panic_payload_message,
    reclaim_after_result_delivery,
};
use super::spawn_outcome::{
    extract_outcome_summary, mark_terminal_in_registry, panic_outcome_summary,
};
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::agent::result_channel::ChildResultSender;
use crate::agent::{PendingAgentMessages, PendingMailboxLease};
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::r#loop::config::AgentLoopConfig;
use crate::r#loop::inbound::{InboundChannel, InboundSender};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::runner::{
    AgentMessageStepRequest, AgentStepRequest, run_agent_step, run_agent_step_from_messages,
};
use crate::r#loop::{
    UndeliveredWindow, persist_undelivered_after_close, requeue_undelivered_inbound,
};
use crate::provider::agent_event::AgentEventSender;
use crate::provider::request::ToolDefinition;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;

pub(super) struct SpawnController {
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
    pub(super) hooks: Option<Arc<HookRegistry>>,
    pub(super) role_label: String,
    pub(super) event_sender: Option<AgentEventSender>,
    pub(super) reclaim: Option<ReclaimHandshake>,
    pub(super) lifecycle: LifecycleEmitter,
    pub(super) router: Arc<MessageRouter>,
    pub(super) child_config: AgentLoopConfig,
    pub(super) run_cancel: tokio_util::sync::CancellationToken,
    pub(super) wake_registry: Option<Arc<AgentWakeRegistry>>,
    pub(super) persistent: bool,
    pub(super) pending_messages: Arc<PendingAgentMessages>,
    pub(super) mailbox_lease: Arc<PendingMailboxLease>,
    pub(super) status_tx: watch::Sender<AgentStatus>,
    pub(super) inbound_tx: InboundSender,
    pub(super) inbound_rx: InboundChannel,
    pub(super) wake_rx: mpsc::Receiver<()>,
    pub(super) wake_pending: Arc<AtomicBool>,
}

impl SpawnController {
    pub(super) async fn run(self) {
        let Self {
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
            hooks,
            role_label,
            event_sender,
            mut reclaim,
            lifecycle,
            router,
            mut child_config,
            run_cancel,
            wake_registry,
            persistent,
            pending_messages,
            mailbox_lease,
            status_tx,
            inbound_tx,
            mut inbound_rx,
            mut wake_rx,
            wake_pending,
        } = self;

        crate::agent::arming::arm_auto_compaction(&mut loop_ctx, &mut child_config, &model);
        let delivered_children = loop_ctx.children_usage.clone();
        let agent_role = format!("spawn/{model}");
        let mut initial = Some(task);
        let mut closed_mailbox = None;

        loop {
            let outcome = if let Some(task) = initial.take() {
                AssertUnwindSafe(run_agent_step(AgentStepRequest {
                    provider: provider.as_ref(),
                    executor: &executor,
                    store: store.as_ref(),
                    user_prompt: &task,
                    tools: &tool_defs,
                    output_schema: output_schema.as_ref(),
                    model: &model,
                    config: &child_config,
                    event_tx: event_sender.as_ref(),
                    inbound: Some(&mut inbound_rx),
                    loop_context: &mut loop_ctx,
                    cancel: Some(run_cancel.clone()),
                }))
                .catch_unwind()
                .await
            } else {
                AssertUnwindSafe(run_agent_step_from_messages(AgentMessageStepRequest {
                    provider: provider.as_ref(),
                    executor: &executor,
                    store: store.as_ref(),
                    tools: &tool_defs,
                    output_schema: output_schema.as_ref(),
                    model: &model,
                    config: &child_config,
                    event_tx: event_sender.as_ref(),
                    initial_messages: Vec::new(),
                    inbound: Some(&mut inbound_rx),
                    loop_context: &mut loop_ctx,
                    cancel: Some(run_cancel.clone()),
                }))
                .catch_unwind()
                .await
            };

            let summary = match outcome {
                Ok(step_outcome) => {
                    extract_outcome_summary(step_outcome, delivered_children.snapshot())
                }
                Err(payload) => {
                    let message = format!(
                        "sub-agent task panicked before completing: {}",
                        panic_payload_message(payload.as_ref()),
                    );
                    tracing::error!(child_id = %child_id, error = %message);
                    panic_outcome_summary(message, delivered_children.snapshot())
                }
            };
            let will_terminate = !persistent
                || run_cancel.is_cancelled()
                || (summary.status == AgentStatus::Failed && summary.stop.is_none());
            match pending_messages.transition_live_route(
                child_id,
                store.as_ref(),
                router.as_ref(),
                &mut inbound_rx,
                will_terminate.then_some(&mailbox_lease),
            ) {
                Ok(transition) => {
                    if let Some(closed) = transition.closed {
                        closed_mailbox = Some(closed);
                    }
                    if let Some(error) = transition.first_error {
                        tracing::error!(
                            child_id = %child_id,
                            %error,
                            "failed to persist one or more messages during route transition",
                        );
                    }
                }
                Err(error) => {
                    tracing::error!(
                        child_id = %child_id,
                        %error,
                        "failed to linearize child route and mailbox transition",
                    );
                    router.deregister(child_id);
                }
            }

            let stop_blocked = if let Some(hooks) = hooks.as_ref() {
                matches!(
                    hooks
                        .run_subagent_stop(&child_id.to_string(), &role_label)
                        .await,
                    HookOutcome::Block { .. },
                )
            } else {
                false
            };

            let subtree_usage = summary.usage.clone() + summary.children_usage.clone();
            let succeeded = summary.status == AgentStatus::Completed;
            if let Err(error) = lifecycle.emit_completed(SubagentCompletion {
                usage: summary.usage.clone(),
                subtree_usage,
                succeeded,
                error: summary.error.clone(),
                stop: summary.stop.clone(),
            }) {
                tracing::error!(
                    child_id = %child_id,
                    %error,
                    "failed to persist the subagent.completed audit event on \
                     the parent store; the child's result is still delivered",
                );
            }
            deliver_step_result(result_sender.as_ref(), child_id, &agent_role, &summary).await;

            if will_terminate {
                if !stop_blocked {
                    mark_terminal_in_registry(&agent_registry, child_id, summary.status);
                }
                let _ = status_tx.send_replace(summary.status);
                if !stop_blocked {
                    reclaim_after_result_delivery(&mut reclaim, &agent_registry, child_id).await;
                }
                break;
            }

            mark_idle(&agent_registry, child_id);
            let _ = status_tx.send_replace(AgentStatus::Idle);

            // A message queued during stop/result handling must restart the
            // child without exposing Active before it has a live route.
            if pending_messages.pending_for(child_id) > 0 {
                activate_route(&router, &inbound_tx, &agent_registry, &status_tx, child_id);
                continue;
            }

            if (IdlePark {
                run_cancel: &run_cancel,
                wake_rx: &mut wake_rx,
                wake_pending: &wake_pending,
                router: &router,
                inbound_tx: &inbound_tx,
                agent_registry: &agent_registry,
                status_tx: &status_tx,
                child_id,
                store: store.as_ref(),
                pending_messages: pending_messages.as_ref(),
                inbound_rx: &mut inbound_rx,
            })
            .wait()
            .await
            {
                inbound_rx.close();
                closed_mailbox = pending_messages.close_child_mailbox(child_id, &mailbox_lease);
                mark_closed(&agent_registry, child_id);
                let _ = status_tx.send_replace(AgentStatus::Closed);
                break;
            }
        }

        // Close the receiver before the final mailbox close/drain so a
        // parent-held direct sender cannot report success after the sweep.
        // `transition_live_route` also closes on ordinary terminal paths;
        // this unconditional close covers its error fallback and park cancel.
        inbound_rx.close();
        if closed_mailbox.is_none() {
            closed_mailbox = pending_messages.close_child_mailbox(child_id, &mailbox_lease);
        }
        router.deregister(child_id);
        let mut stranded = inbound_rx.drain();
        if let Some(closed) = closed_mailbox.as_ref()
            && let Err(error) = persist_undelivered_after_close(
                pending_messages.as_ref(),
                closed,
                &mut stranded,
                UndeliveredWindow::Deregistration,
            )
        {
            tracing::error!(
                child_id = %child_id,
                %error,
                "failed to persist child messages accepted before terminal mailbox closure",
            );
        }
        if let Some(registry) = wake_registry {
            registry.remove(child_id);
        }
    }
}

struct IdlePark<'a> {
    run_cancel: &'a tokio_util::sync::CancellationToken,
    wake_rx: &'a mut mpsc::Receiver<()>,
    wake_pending: &'a AtomicBool,
    router: &'a MessageRouter,
    inbound_tx: &'a InboundSender,
    agent_registry: &'a RwLock<AgentRegistry>,
    status_tx: &'a watch::Sender<AgentStatus>,
    child_id: Uuid,
    store: &'a EventStore,
    pending_messages: &'a PendingAgentMessages,
    inbound_rx: &'a mut InboundChannel,
}

impl IdlePark<'_> {
    async fn wait(&mut self) -> bool {
        let mut inbound_open = true;
        loop {
            tokio::select! {
                biased;
                () = self.run_cancel.cancelled() => return true,
                wake = self.wake_rx.recv() => {
                    if wake.is_none() {
                        return true;
                    }
                    activate_route(
                        self.router,
                        self.inbound_tx,
                        self.agent_registry,
                        self.status_tx,
                        self.child_id,
                    );
                    self.wake_pending.store(false, Ordering::SeqCst);
                    return false;
                }
                received = self.inbound_rx.recv(), if inbound_open => {
                    match received {
                        Some(message) => {
                            let mut stranded = vec![message];
                            stranded.extend(self.inbound_rx.drain());
                            if let Err(error) = requeue_undelivered_inbound(
                                self.store,
                                Some(self.child_id),
                                Some(self.pending_messages),
                                &mut stranded,
                                UndeliveredWindow::IdlePark,
                            ) {
                                tracing::error!(
                                    child_id = %self.child_id,
                                    %error,
                                    "failed to persist queued audit event(s) for messages \
                                     received while parked; affected messages will not \
                                     survive a restart",
                                );
                            }
                        }
                        None => inbound_open = false,
                    }
                }
            }
        }
    }
}
