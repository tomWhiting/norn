//! Application state and event loop.

pub mod autocomplete;
pub mod dispatch;
pub mod edit;
pub mod event_loop;
pub mod helpers;
pub mod render;
pub mod rotation;
pub mod slash;
pub mod state;

pub use event_loop::{TuiInputs, run_app};
pub use state::AppState;
