//! Print-mode execution — driving the agent step and rendering output.

mod driven;
pub mod intervene;
pub mod jsonrpc;
pub mod orchestrator;
pub mod output;
pub mod provider;
mod provider_trace;
mod step_output;

pub use orchestrator::{run, run_async};
pub use output::{
    ENVELOPE_VERSION, JsonEnvelope, StopInfo, StreamRendererHandle, UsageOut, drain_diagnostics,
    emit_stream_completed, extract_output_and_usage, render_json, render_text,
    spawn_stream_renderer,
};
pub use provider::{BuiltProvider, ProviderBuildError, build_provider};
