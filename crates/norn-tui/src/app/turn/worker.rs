//! Own one turn off the terminal task; return the exact context and inbound receiver.

use std::sync::Arc;

use crate::TuiError;
use norn::agent_loop::LoopContext;
use norn::agent_loop::config::AgentLoopConfig;
use norn::agent_loop::inbound::InboundChannel;
use norn::agent_loop::runner::{
    AgentMessageStepRequest, AgentStepRequest, AgentStepResult, ToolExecutor, run_agent_step,
    run_agent_step_from_messages,
};
use norn::error::NornError;
use norn::provider::AgentEventSender;
use norn::provider::request::ToolDefinition;
use norn::provider::traits::Provider;
use norn::session::EventStore;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::app::event_loop::RuntimeRefs;

use super::super::seed::TurnSeed;

pub(super) struct CompletedTurn {
    context: LoopContext,
    inbound: Option<InboundChannel>,
    result: Result<AgentStepResult, NornError>,
}

impl CompletedTurn {
    pub(super) fn restore(self, runtime: &mut RuntimeRefs) -> Result<AgentStepResult, NornError> {
        runtime.loop_context = self.context;
        runtime.root_inbound = self.inbound;
        self.result
    }
}

struct OwnedTurn {
    context: LoopContext,
    inbound: Option<InboundChannel>,
    provider: Arc<dyn Provider>,
    executor: Arc<dyn ToolExecutor>,
    store: Arc<EventStore>,
    model: String,
    config: AgentLoopConfig,
    tools: Vec<ToolDefinition>,
    sender: AgentEventSender,
    cancel: CancellationToken,
}

/// Move the execution owners, never clone the mutable context or recreate inboxes.
/// The calling turn holds exclusive runtime access until it restores completion.
pub(super) fn start(
    runtime: &mut RuntimeRefs,
    seed: TurnSeed,
    sender: AgentEventSender,
    cancel: CancellationToken,
) -> Result<ExecutionWorker, TuiError> {
    let owned = OwnedTurn {
        context: std::mem::take(&mut runtime.loop_context),
        inbound: runtime.root_inbound.take(),
        provider: Arc::clone(&runtime.provider),
        executor: Arc::clone(&runtime.executor),
        store: Arc::clone(&runtime.store),
        model: runtime.model.clone(),
        config: runtime.agent_config.clone(),
        tools: runtime.tools.clone(),
        sender,
        cancel,
    };
    launch(owned, seed)
}

fn launch(owned: OwnedTurn, seed: TurnSeed) -> Result<ExecutionWorker, TuiError> {
    // A plain async task still shares current-thread hosts. Holding their blocking
    // pool for the whole turn can deadlock nested persistence with a one-slot pool.
    // This owned thread uses the existing runtime drivers and exists only for the turn.
    let handle = tokio::runtime::Handle::current();
    let cancellation = owned.cancel.clone();
    let (completion_tx, receive) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("norn-turn".into())
        .spawn(move || {
            let completed = handle.block_on(owned.run(seed));
            if completion_tx.send(completed).is_err() {
                tracing::debug!(
                    "terminal owner closed before completed turn state could be returned"
                );
            }
        })
        .map_err(|source| TuiError::ExecutionTask { source })?;
    Ok(ExecutionWorker {
        receive,
        thread: Some(thread),
        cancellation,
    })
}

/// Own the active execution thread and its one completion; never an agent process.
pub(super) struct ExecutionWorker {
    receive: oneshot::Receiver<CompletedTurn>,
    thread: Option<std::thread::JoinHandle<()>>,
    cancellation: CancellationToken,
}

impl ExecutionWorker {
    pub(super) async fn wait(&mut self) -> Result<CompletedTurn, TuiError> {
        let completed = (&mut self.receive).await;
        // The completion is sent as the thread's final operation. Join only after
        // that notification (or sender loss on panic), never while executing work.
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|payload| {
                let reason = if let Some(message) = payload.downcast_ref::<String>() {
                    message.clone()
                } else if let Some(message) = payload.downcast_ref::<&str>() {
                    (*message).to_owned()
                } else {
                    "non-text panic payload".to_owned()
                };
                TuiError::ExecutionTask {
                    source: std::io::Error::other(format!("norn-turn thread panicked: {reason}")),
                }
            })?;
        }
        completed.map_err(|source| TuiError::ExecutionTask {
            source: std::io::Error::other(format!(
                "norn-turn completed without returning session state: {source}"
            )),
        })
    }
}

impl Drop for ExecutionWorker {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl OwnedTurn {
    async fn run(mut self, seed: TurnSeed) -> CompletedTurn {
        let result = match seed {
            TurnSeed::Operator(crate::app::transcript::publication::SubmittedInput {
                text: prompt,
                ..
            })
            | TurnSeed::ChildResult(prompt) => {
                run_agent_step(AgentStepRequest {
                    provider: self.provider.as_ref(),
                    executor: &self.executor,
                    store: self.store.as_ref(),
                    user_prompt: &prompt,
                    tools: &self.tools,
                    output_schema: None,
                    model: &self.model,
                    config: &self.config,
                    event_tx: Some(&self.sender),
                    inbound: self.inbound.as_mut(),
                    loop_context: &mut self.context,
                    cancel: Some(self.cancel.clone()),
                })
                .await
            }
            TurnSeed::AgentMessages(initial_messages) => self.run_messages(initial_messages).await,
            TurnSeed::McpChannelWake => self.run_messages(Vec::new()).await,
        };
        CompletedTurn {
            context: self.context,
            inbound: self.inbound,
            result,
        }
    }

    async fn run_messages(
        &mut self,
        initial_messages: Vec<norn::agent_loop::inbound::ChannelMessage>,
    ) -> Result<AgentStepResult, NornError> {
        run_agent_step_from_messages(AgentMessageStepRequest {
            provider: self.provider.as_ref(),
            executor: &self.executor,
            store: self.store.as_ref(),
            tools: &self.tools,
            output_schema: None,
            model: &self.model,
            config: &self.config,
            event_tx: Some(&self.sender),
            initial_messages,
            inbound: self.inbound.as_mut(),
            loop_context: &mut self.context,
            cancel: Some(self.cancel.clone()),
        })
        .await
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
