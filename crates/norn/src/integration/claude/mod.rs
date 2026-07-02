//! Claude Runner integration and Norn-wrapped Claude Code.
//!
//! Two integration modes live here:
//!
//! - [`ClaudeRunnerAdapter`] — implements [`Provider`](crate::provider::traits::Provider) over the Claude Code
//!   CLI by spawning a [`claude_runner::ClaudeProcess`] and translating its
//!   stream-json events into [`ProviderEvent`](crate::provider::events::ProviderEvent)s. Callers route agent steps
//!   through the legitimate Claude subscription path. The adapter is paired
//!   with [`StepOutcome`], the structured return value produced by a single
//!   step.
//!
//! - [`NornWrappedClaudeCode`] — launches Claude Code stripped to bare metal
//!   (no native tools, replaced system prompt), exposes Norn's tools via an
//!   MCP config, and captures every stream-json event back into Norn's
//!   session-event format.

mod adapter;
mod wrapped;

pub use adapter::{ClaudeRunnerAdapter, ClaudeRunnerConfig, StepOutcome};
pub use wrapped::{NornWrappedClaudeCode, NornWrappedClaudeConfig};
