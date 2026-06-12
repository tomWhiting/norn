//! Agent loop: prompt-tool cycle, schema enforcement, streaming events.

pub use crate::error::SchemaError;

pub use crate::r#loop::children_usage::ChildrenUsage;
pub use crate::r#loop::commands::{
    CustomSlashHandler, PreprocessResult, SlashCommand, SlashCommandHandler, SlashCommandRegistry,
    preprocess_input,
};
pub use crate::r#loop::compaction::{
    AutoCompactArgs, AutoCompactionRun, CompactionState, CompactionSummarySource, TimeoutState,
    maybe_auto_compact,
};
pub use crate::r#loop::iteration::{
    IterationMonitorConfig, IterationMonitorState, IterationSignal, QualitySignal,
};
pub use crate::r#loop::linger::LingerPolicy;
pub use crate::r#loop::loop_context::LoopContext;

pub use crate::r#loop::retry::{RetryPolicy, RetryableError, retry_with_backoff};

pub use crate::r#loop::tokens::{SimpleTokenEstimator, TokenEstimator, estimate_prompt_tokens};

pub mod assembly;
pub mod children_usage;
mod classify;

pub mod commands;
pub mod compaction;
pub mod config;
pub mod context;
mod conversation_state;
mod delivery;
mod dev_context;
pub mod event_schemas;
pub mod events;

pub mod expansion;
mod failure_tracking;
mod helpers;
mod inflight_compaction;
pub use helpers::ensure_tool_results_complete;
pub mod inbound;
pub mod iteration;
pub mod linger;
pub mod loop_context;
pub mod notifications;
mod numeric;

pub mod retry;
mod rule_wiring;
pub mod runner;
pub mod schema;
mod summarization;
mod tool_dispatch;

pub mod tokens;
