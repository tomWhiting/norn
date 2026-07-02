//! Per-tool renderers — rich, compact, and minimal tiers.

pub mod compact;
mod helpers;
pub mod minimal;
pub mod renderer;
pub mod rich;
pub mod status;
mod verbosity;

pub use renderer::{ToolRenderer, renderer_for};
pub use verbosity::VerbosityState;
