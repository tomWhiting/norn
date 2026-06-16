//! Application state and event loop.

pub mod autocomplete;
pub mod child_results;
pub mod dispatch;
pub mod edit;
pub mod event_loop;
pub mod helpers;
pub mod render;
pub mod rotation;
mod session_replay;
pub mod slash;
mod slash_catalog;
pub mod state;
pub mod streaming;
pub mod tool_calls;
mod turn;

pub use event_loop::{TuiInputs, run_app};
pub use state::AppState;
