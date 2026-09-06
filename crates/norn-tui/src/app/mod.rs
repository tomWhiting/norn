//! Application state and event loop.

pub mod active_input;
pub mod autocomplete;
mod changes;
pub mod child_results;
pub mod dispatch;
pub mod edit;
pub mod event_loop;
mod export;
mod focus;
mod frontend_preferences;
pub mod helpers;
mod mcp_slash;
mod model_selection;
pub mod notices;
pub mod render;
pub mod rotation;
mod search;
mod selection;
mod session_replay;
pub mod slash;
mod slash_catalog;
pub mod state;
pub mod streaming;
pub mod tool_calls;
pub mod transcript;
mod turn;
mod view_actions;
pub mod view_config;
mod viewport;

pub use event_loop::{TuiInputs, run_app};
pub use state::AppState;
