//! Print-mode execution — driving the agent step and rendering output.

mod driven;
pub mod intervene;
pub mod jsonrpc;
pub mod orchestrator;
pub mod output;
pub mod provider;
mod provider_trace;
mod step_output;
pub mod stream_renderer;

pub use orchestrator::{run, run_async};
pub use output::{
    ENVELOPE_VERSION, JsonEnvelope, StopInfo, UsageOut, drain_diagnostics, emit_stream_completed,
    extract_output_and_usage, render_json, render_text,
};
pub use provider::{BuiltProvider, ProviderBuildError, build_provider};
pub use stream_renderer::{StreamRendererHandle, spawn_stream_renderer};
