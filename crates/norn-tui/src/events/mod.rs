//! Session event rendering and structured output dispatch.

mod display_toggles;
pub mod schema_render;

pub use display_toggles::DisplayToggles;
pub use schema_render::{
    render_assistant_message, render_event, render_structured, render_thinking, render_tool_call,
    render_user_message,
};
