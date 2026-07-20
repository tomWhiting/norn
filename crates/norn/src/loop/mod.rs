//! Agent loop: prompt-tool cycle, schema enforcement, streaming events.

pub use crate::error::SchemaError;

pub use crate::r#loop::active_input::{
    ActiveInputDelivery, ActiveInputDeliveryReceiver, ActiveInputError, ActiveInputReceiver,
    ActiveInputSender, active_input_channel,
};
pub use crate::r#loop::children_usage::ChildrenUsage;
pub use crate::r#loop::commands::{
    BuiltinSlashCommand, BuiltinSlashKind, CustomSlashHandler, EffortCommand, PreprocessResult,
    ServiceTierCommand, SlashCommand, SlashCommandHandler, SlashCommandRegistry, SlashSurface,
    builtin_slash_commands, effort_label, find_builtin_slash_command, parse_effort_command,
    parse_service_tier_command, preprocess_input, reasoning_effort_supported_for_model,
    service_tier_supported_for_model, unsupported_reasoning_effort_message,
    unsupported_service_tier_message,
};
pub use crate::r#loop::compaction::{
    AutoCompactArgs, AutoCompactionRun, CompactionState, CompactionSummarySource,
    ManualCompactionEstimate, TimeoutState, estimate_manual_compaction, maybe_auto_compact,
};
pub use crate::r#loop::iteration::{
    IterationMonitorConfig, IterationMonitorState, IterationSignal, QualitySignal,
};
pub use crate::r#loop::linger::LingerPolicy;
pub use crate::r#loop::loop_context::LoopContext;

pub use crate::r#loop::retry::{RetryPolicy, RetryableError, retry_with_backoff};

pub use crate::r#loop::tokens::{SimpleTokenEstimator, TokenEstimator, estimate_prompt_tokens};

pub mod active_input;
pub mod assembly;
pub mod children_usage;
mod classify;

pub mod commands;
pub mod compaction;
pub mod config;
pub mod context;
mod conversation_state;
mod delivery;
mod delivery_inputs;
mod dev_context;
pub mod event_schemas;

pub mod expansion;
mod failure_tracking;
mod helpers;
mod inflight_compaction;
pub(crate) use delivery::{UndeliveredWindow, requeue_undelivered_inbound};
pub(crate) use helpers::append_off_executor;
pub use tool_result_repair::ensure_tool_results_complete;
pub mod inbound;
pub mod iteration;
pub mod linger;
pub mod loop_context;
pub mod notifications;
mod numeric;
mod programmatic_calling;
mod response_audio_capture;
mod response_publication;

pub mod retry;
mod rule_wiring;
pub mod runner;
pub mod schema;
mod stop_records;
mod summarization;
mod timeout_state;
mod tool_dispatch;
mod tool_result_repair;

pub mod tokens;

#[cfg(test)]
mod canonical_tool_resolution_tests;

#[cfg(test)]
mod caller_propagation_tests;

#[cfg(test)]
mod classify_audio_tests;

#[cfg(test)]
mod response_audio_end_to_end_tests;

#[cfg(test)]
mod response_audio_lifecycle_loop_tests;

#[cfg(test)]
mod unsupported_response_loop_tests;
