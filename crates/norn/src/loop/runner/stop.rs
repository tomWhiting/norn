//! Stop-boundary phase: the single implementation of the would-stop
//! sequence every Completed-return path funnels through — boundary
//! resolution (inbound/child sweep plus optional linger), the Stop hook,
//! and completion envelope construction.
//!
//! Historically this sequence was duplicated inline at each of the three
//! completed exits (schema-valid, text-stop, tools+schema-valid). It now
//! exists exactly once: [`StepMachine::resolve_stop`]. Per `DESIGN.md` D5
//! wiring and NH-006 R4 acceptance, the
//! [`HookRegistry::run_stop`](crate::integration::hooks::HookRegistry::run_stop)
//! call still guards every Completed return, because every such return is
//! this one.

use serde_json::Value;

use crate::error::NornError;
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::r#loop::config::AgentStepResult;
use crate::r#loop::helpers::append_and_notify;
use crate::r#loop::linger::{BoundaryOutcome, StopBoundary, resolve_stop_boundary};
use crate::provider::request::{Message, MessageRole};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

use super::machine::{StepFlow, StepMachine, StepState};

/// The completion payload carried into the stop boundary.
pub(super) enum StopOutput {
    /// Structured completion: the validated schema-tool output, plus the
    /// assistant text the Stop hook observes.
    Structured {
        /// Validated output of the schema tool call.
        output: Value,
        /// Assistant text of the final turn, passed to the Stop hook.
        text: String,
    },
    /// Plain-text completion (no output schema): the assistant text is
    /// both the Stop hook input and the step output.
    Text(String),
}

impl StopOutput {
    /// The assistant text the Stop hook observes.
    fn hook_text(&self) -> &str {
        match self {
            Self::Structured { text, .. } | Self::Text(text) => text,
        }
    }

    /// The step's final output value.
    fn into_value(self) -> Value {
        match self {
            Self::Structured { output, .. } => output,
            Self::Text(text) => Value::String(text),
        }
    }
}

impl StepMachine<'_> {
    /// Resolve one would-stop boundary and complete the step, unless
    /// injected work (boundary sweep, linger wake, or a Stop-hook block)
    /// sends the loop around again.
    pub(super) async fn resolve_stop(&mut self, stop: StopOutput) -> Result<StepFlow, NornError> {
        match resolve_stop_boundary(StopBoundary {
            store: self.store,
            messages: &mut self.messages,
            inbound: self.inbound.as_deref_mut(),
            follow_up_buffer: &mut *self.follow_up_buffer,
            loop_context: &mut *self.loop_context,
            linger: self.config.linger,
            cancel: self.cancel.as_ref(),
            event_tx: self.event_tx,
        })
        .await?
        {
            BoundaryOutcome::Continue => return Ok(StepFlow::Next(StepState::Gate)),
            BoundaryOutcome::Cancelled => return Ok(StepFlow::Done(self.cancelled_result())),
            BoundaryOutcome::Stop => {}
        }

        if let Some(hooks) = self.loop_context.hooks.as_deref() {
            let outcome = hooks.run_stop(stop.hook_text()).await;
            if inject_stop_block(outcome, hooks, self.store, &mut self.messages).await? {
                return Ok(StepFlow::Next(StepState::Gate));
            }
        }

        Ok(StepFlow::Done(AgentStepResult::Completed {
            output: stop.into_value(),
            usage: std::mem::take(&mut self.total_usage),
            children_usage: self.loop_context.children_usage.snapshot(),
        }))
    }
}

/// On a [`HookOutcome::Block`] from `outcome`, inject the supplied
/// reason as a follow-up user message — both into the session event
/// store and into the live `messages` vec — so the next iteration
/// re-runs the model with the block reason in context. Returns `true`
/// when a Block was injected (caller must continue the loop instead of
/// completing), `false` otherwise.
async fn inject_stop_block(
    outcome: HookOutcome,
    hooks: &HookRegistry,
    store: &EventStore,
    messages: &mut Vec<Message>,
) -> Result<bool, NornError> {
    let HookOutcome::Block { reason } = outcome else {
        return Ok(false);
    };
    append_and_notify(
        store,
        SessionEvent::UserMessage {
            base: EventBase::new(store.last_event_id()),
            content: reason.clone(),
        },
        Some(hooks),
    )
    .await?;
    messages.push(Message {
        response_items: Vec::new(),
        role: MessageRole::User,
        content: Some(reason),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
    });
    Ok(true)
}
