//! Agent loop runner: prompt-tool cycle with schema enforcement.
//!
//! This module contains the core agent loop that drives a single step of
//! agent execution. It sends prompts to a provider, collects streaming
//! events, executes tool calls, and enforces structured output schemas
//! with a bounded retry budget.
//!
//! The step is an explicit state machine
//! ([`machine::StepMachine`]/[`machine::StepState`]): the iteration gate
//! lives in [`machine`], the pre-loop setup in [`setup`], request
//! assembly in [`prompt`], the provider call in [`provider_call`],
//! response routing in [`dispatch`], and the deduplicated would-stop
//! boundary in [`stop`]. The public entry points and request types live
//! in [`entry`].
//!
//! Configuration types ([`AgentLoopConfig`], [`AgentStepResult`],
//! [`ToolExecutor`]) live in the sibling [`crate::r#loop::config`] module
//! and are re-exported here for backward compatibility.

mod dispatch;
mod entry;
mod machine;
mod prompt;
mod provider_call;
mod setup;
mod stop;
#[cfg(test)]
mod tests;

pub use entry::{
    AgentMessageStepRequest, AgentStepRequest, run_agent_step, run_agent_step_from_messages,
};

pub use crate::r#loop::config::{
    AgentLoopConfig, AgentStepResult, ToolExecutor, TruncationKind, driver_executor,
};

#[cfg(any(test, feature = "test-utils"))]
pub use crate::r#loop::config::{MockToolExecutor, ToolHandler};
