//! Agent loop: prompt-tool cycle, schema enforcement, streaming events.

pub use crate::error::SchemaError;

pub use crate::r#loop::commands::{
    CustomSlashHandler, PreprocessResult, SlashCommand, SlashCommandHandler, SlashCommandRegistry,
    preprocess_input,
};
pub use crate::r#loop::compaction::{CompactionState, TimeoutState, maybe_auto_compact};
pub use crate::r#loop::iteration::{
    IterationMonitorConfig, IterationMonitorState, IterationSignal, QualitySignal,
};
pub use crate::r#loop::loop_context::LoopContext;

pub use crate::r#loop::retry::{RetryPolicy, RetryableError, retry_with_backoff};

pub use crate::r#loop::tokens::{SimpleTokenEstimator, TokenEstimator, estimate_prompt_tokens};

pub mod assembly;
mod classify;

pub mod commands;
pub mod compaction;
pub mod config;
pub mod context;
mod conversation_state;
pub mod event_schemas;
pub mod events;

pub mod expansion;
mod helpers;
pub use helpers::ensure_tool_results_complete;
pub mod inbound;
pub mod iteration;
pub mod loop_context;
pub mod notifications;

pub mod retry;
mod rule_wiring;
pub mod runner;
pub mod schema;
mod tool_dispatch;

pub mod tokens;
