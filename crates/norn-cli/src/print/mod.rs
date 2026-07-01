//! Print-mode execution — driving the agent step and rendering output.

pub mod intervene;
pub mod jsonrpc;
pub mod orchestrator;
pub mod output;
pub mod provider;
mod provider_trace;
pub mod session;

pub use orchestrator::{run, run_async};
pub use output::{
    JsonEnvelope, StreamRendererHandle, UsageOut, drain_diagnostics, emit_stream_completed,
    extract_output_and_usage, render_json, render_text, result_label, spawn_stream_renderer,
};
pub use provider::{BuiltProvider, ProviderBuildError, build_provider};
